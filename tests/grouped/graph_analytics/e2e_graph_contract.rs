use reddb::application::ports::RuntimeEntityPort;
use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, DeleteEntityInput,
    MergeInput, MergeOpts, PatchEntityInput, ResetInput, ResetMode, StatusInput,
};
use reddb::catalog::{
    GraphContract, GraphEdgeContract, GraphPropertyContract, GraphVertexContract,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::collections::BTreeMap;

const CONNECTION: u64 = 9501;

fn contract() -> GraphContract {
    GraphContract {
        vertex_types: BTreeMap::from([
            (
                "Person".into(),
                GraphVertexContract {
                    properties: BTreeMap::from([(
                        "age".into(),
                        GraphPropertyContract {
                            data_type: "INT".into(),
                            required: true,
                        },
                    )]),
                },
            ),
            ("Company".into(), GraphVertexContract::default()),
        ]),
        edge_types: BTreeMap::from([(
            "WORKS_AT".into(),
            GraphEdgeContract {
                from_type: "Person".into(),
                to_type: "Company".into(),
                properties: BTreeMap::new(),
            },
        )]),
    }
}

fn setup(runtime: &RedDBRuntime, versioned: bool) {
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    if versioned {
        runtime.vcs_set_versioned("people", true).unwrap();
    }
    runtime
        .set_graph_contract("people", Some(contract()))
        .unwrap();
    runtime.execute_query("INSERT INTO people NODE (label,node_type,age) VALUES ('alice','Person',30),('acme','Company',NULL)").unwrap();
}

fn edge(runtime: &RedDBRuntime) {
    runtime
        .execute_query(
            "INSERT INTO people EDGE (label,\"from\",\"to\") VALUES ('WORKS_AT','alice','acme')",
        )
        .unwrap();
}

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}
fn commit(runtime: &RedDBRuntime) -> String {
    runtime
        .vcs_commit(CreateCommitInput {
            connection_id: CONNECTION,
            message: "checkpoint".into(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap()
        .hash
}
fn checkout(runtime: &RedDBRuntime, branch: &str) {
    runtime
        .vcs_checkout(CheckoutInput {
            connection_id: CONNECTION,
            target: CheckoutTarget::Branch(branch.into()),
            force: false,
        })
        .unwrap();
}

#[test]
fn optional_contract_validates_existing_data_and_rejects_invalid_batches() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    runtime
        .execute_query("INSERT INTO people NODE (label,node_type) VALUES ('legacy','Unknown')")
        .unwrap();
    assert!(runtime
        .set_graph_contract("people", Some(contract()))
        .is_err());
    assert!(runtime.graph_contract("people").unwrap().is_none());
    runtime.execute_query("DELETE FROM people").unwrap();
    runtime
        .set_graph_contract("people", Some(contract()))
        .unwrap();
    for sql in [
        "INSERT INTO people NODE (label,node_type,age) VALUES ('valid','Person',30),('bad','Unknown',30)",
        "INSERT INTO people NODE (label,node_type) VALUES ('missing','Person')",
        "INSERT INTO people NODE (label,node_type,age) VALUES ('null','Person',NULL)",
        "INSERT INTO people NODE (label,node_type,age) VALUES ('wrong','Person','thirty')",
        "INSERT INTO people NODE (label,node_type,age) VALUES ('Person',['Person','Company'],30)",
    ] {
        assert!(runtime.execute_query(sql).is_err(), "{sql}");
        assert!(runtime.scan_collection("people", None, 20).unwrap().items.is_empty(), "atomic rejection: {sql}");
    }
    runtime.set_graph_contract("people", None).unwrap();
    runtime
        .execute_query("INSERT INTO people NODE (label,node_type) VALUES ('legacy','Unknown')")
        .unwrap();
    clear_current_connection_id();
}

#[test]
fn graph_contract_does_not_resolve_endpoints_from_another_tenant() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    runtime.register_tenant_table("people", "tenant_id");
    runtime
        .set_graph_contract("people", Some(contract()))
        .unwrap();
    let alice = runtime
        .db()
        .node("people", "alice")
        .node_type("Person")
        .property("age", 30i64)
        .property("tenant_id", "acme")
        .save()
        .unwrap();
    let company = runtime
        .db()
        .node("people", "company")
        .node_type("Company")
        .property("tenant_id", "globex")
        .save()
        .unwrap();
    assert!(runtime
        .db()
        .edge("people", "WORKS_AT")
        .from(alice)
        .to(company)
        .property("tenant_id", "acme")
        .save()
        .is_err());
    let local = runtime
        .db()
        .node("people", "company")
        .node_type("Company")
        .property("tenant_id", "acme")
        .save()
        .unwrap();
    runtime
        .db()
        .edge("people", "WORKS_AT")
        .from(alice)
        .to(local)
        .property("tenant_id", "acme")
        .save()
        .unwrap();
    clear_current_connection_id();
}

#[test]
fn graph_contract_rejects_invalid_history_as_new_head() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    runtime.vcs_set_versioned("people", true).unwrap();
    runtime
        .execute_query("INSERT INTO people NODE (label,node_type) VALUES ('bad','Unknown')")
        .unwrap();
    let invalid = commit(&runtime);
    runtime.execute_query("DELETE FROM people").unwrap();
    let valid = commit(&runtime);
    runtime
        .set_graph_contract("people", Some(contract()))
        .unwrap();
    assert!(runtime
        .vcs_reset(ResetInput {
            connection_id: CONNECTION,
            target: invalid.clone(),
            mode: ResetMode::Hard
        })
        .is_err());
    assert_eq!(
        runtime
            .vcs_status(StatusInput {
                connection_id: CONNECTION
            })
            .unwrap()
            .head_commit,
        Some(valid)
    );
    assert_eq!(
        runtime
            .execute_query(&format!("SELECT * FROM people AS OF COMMIT '{invalid}'"))
            .unwrap()
            .result
            .records
            .len(),
        1
    );
    clear_current_connection_id();
}

#[test]
fn graph_contract_definition_checks_types_endpoints_and_json_shape() {
    let expected = contract();
    assert_eq!(
        GraphContract::from_json(&expected.to_json().unwrap()).unwrap(),
        expected
    );
    let mut invalid = expected.clone();
    invalid.edge_types.get_mut("WORKS_AT").unwrap().to_type = "Missing".into();
    assert!(invalid.validate().is_err());
    let mut invalid = expected.clone();
    invalid
        .vertex_types
        .get_mut("Person")
        .unwrap()
        .properties
        .get_mut("age")
        .unwrap()
        .data_type = "NOT_A_TYPE".into();
    assert!(invalid.validate().is_err());
    for json in [
        r#"{"vertex_types":{}}"#,
        r#"{"vertex_types":{"Person":{}},"inheritance":true}"#,
        r#"{"vertex_types":{"Person":{"properties":{"age":{"data_type":"INT","required":"yes"}}}}}"#,
    ] {
        assert!(GraphContract::from_json(json).is_err(), "{json}");
    }
}

#[test]
fn endpoints_and_native_mutations_enforce_one_concrete_type() {
    for versioned in [false, true] {
        let runtime = RedDBRuntime::in_memory().unwrap();
        setup(&runtime, versioned);
        edge(&runtime);
        for sql in [
            "INSERT INTO people EDGE (label,\"from\",\"to\") VALUES ('WORKS_AT','acme','alice')",
            "INSERT INTO people EDGE (label,\"from\",\"to\") VALUES ('UNKNOWN','alice','acme')",
            "UPDATE people NODES SET node_type='Company' WHERE label='alice'",
            "UPDATE people NODES SET age='invalid' WHERE label='alice'",
            "DELETE FROM people WHERE label='acme'",
        ] {
            assert!(runtime.execute_query(sql).is_err(), "{sql}");
        }
        let alice = runtime.scan_collection("people", None, 20).unwrap().items.into_iter().find(|entity| matches!(&entity.kind, reddb::storage::EntityKind::GraphNode(node) if node.label == "alice")).unwrap();
        assert!(runtime
            .delete_entity(DeleteEntityInput {
                collection: "people".into(),
                id: alice.logical_id()
            })
            .is_err());
        runtime
            .patch_entity(PatchEntityInput {
                collection: "people".into(),
                id: alice.logical_id(),
                payload: reddb::json::from_str(r#"{"fields":{"age":31}}"#).unwrap(),
                operations: vec![],
            })
            .unwrap();
        assert!(runtime
            .patch_entity(PatchEntityInput {
                collection: "people".into(),
                id: alice.logical_id(),
                payload: reddb::json::from_str(r#"{"fields":{"age":"bad"}}"#).unwrap(),
                operations: vec![]
            })
            .is_err());
        assert!(runtime
            .db()
            .node("people", "bad")
            .node_type("Person")
            .property("age", "wrong")
            .save()
            .is_err());
        assert!(runtime
            .db()
            .node("people", "bob")
            .node_type("Person")
            .property("age", 25i64)
            .link_to(alice.logical_id(), "WORKS_AT")
            .save()
            .is_err());
        assert_eq!(
            runtime
                .scan_collection("people", None, 20)
                .unwrap()
                .items
                .len(),
            3
        );
        runtime.execute_query("BEGIN").unwrap();
        runtime.execute_query("DELETE FROM people").unwrap();
        assert!(runtime
            .scan_collection("people", None, 20)
            .unwrap()
            .items
            .is_empty());
        runtime.execute_query("ROLLBACK").unwrap();
        assert_eq!(
            runtime
                .scan_collection("people", None, 20)
                .unwrap()
                .items
                .len(),
            3
        );
        clear_current_connection_id();
    }
}

#[test]
fn graph_contract_rejects_commit_write_skew() {
    for edge_first in [false, true] {
        let runtime = RedDBRuntime::in_memory().unwrap();
        setup(&runtime, false);
        runtime.execute_query("BEGIN").unwrap();
        set_current_connection_id(CONNECTION + 1);
        runtime.execute_query("BEGIN").unwrap();
        runtime
            .execute_query("DELETE FROM people WHERE label='acme'")
            .unwrap();
        set_current_connection_id(CONNECTION);
        edge(&runtime);
        if edge_first {
            runtime.execute_query("COMMIT").unwrap();
            set_current_connection_id(CONNECTION + 1);
        } else {
            set_current_connection_id(CONNECTION + 1);
            runtime.execute_query("COMMIT").unwrap();
            set_current_connection_id(CONNECTION);
        }
        assert!(
            runtime.execute_query("COMMIT").is_err(),
            "second commit must reject a dangling edge"
        );
        assert_eq!(
            runtime
                .scan_collection("people", None, 20)
                .unwrap()
                .items
                .len(),
            if edge_first { 3 } else { 1 }
        );
        clear_current_connection_id();
    }
}

#[test]
fn graph_contract_is_durable_for_embedded_and_paged_storage() {
    for paged in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.rdb");
        let options = || {
            let options = RedDBOptions::persistent(&path);
            if paged {
                options
                    .with_storage_profile(StorageDeployPreset::Serverless.selection())
                    .unwrap()
            } else {
                options
            }
        };
        {
            let runtime = RedDBRuntime::with_options(options()).unwrap();
            setup(&runtime, true);
            edge(&runtime);
            runtime.checkpoint().unwrap();
        }
        let runtime = RedDBRuntime::with_options(options()).unwrap();
        assert_eq!(runtime.graph_contract("people").unwrap(), Some(contract()));
        assert!(runtime
            .execute_query("DELETE FROM people WHERE label='acme'")
            .is_err());
        runtime
            .execute_query("UPDATE people NODES SET age=32 WHERE label='alice'")
            .unwrap();
        clear_current_connection_id();
    }
}

#[test]
fn merge_revalidates_combined_graph_without_advancing_head() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    setup(&runtime, true);
    commit(&runtime);
    runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: CONNECTION,
            name: "agent".into(),
            from: None,
        })
        .unwrap();
    checkout(&runtime, "agent");
    edge(&runtime);
    commit(&runtime);
    checkout(&runtime, "main");
    runtime
        .execute_query("DELETE FROM people WHERE label='acme'")
        .unwrap();
    let head = commit(&runtime);
    assert!(runtime
        .vcs_merge(MergeInput {
            connection_id: CONNECTION,
            from: "agent".into(),
            opts: MergeOpts::default(),
            author: author()
        })
        .is_err());
    assert_eq!(
        runtime
            .vcs_status(StatusInput {
                connection_id: CONNECTION
            })
            .unwrap()
            .head_commit,
        Some(head)
    );
    assert_eq!(
        runtime
            .scan_collection("people", None, 20)
            .unwrap()
            .items
            .len(),
        1
    );
    clear_current_connection_id();
}

#[test]
fn enabling_contract_validates_all_live_branches() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    runtime.vcs_set_versioned("people", true).unwrap();
    commit(&runtime);
    runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: CONNECTION,
            name: "agent".into(),
            from: None,
        })
        .unwrap();
    checkout(&runtime, "agent");
    runtime
        .execute_query("INSERT INTO people NODE (label,node_type) VALUES ('bad','Unknown')")
        .unwrap();
    checkout(&runtime, "main");
    assert!(runtime
        .set_graph_contract("people", Some(contract()))
        .is_err());
    assert!(runtime.graph_contract("people").unwrap().is_none());
    clear_current_connection_id();
}

fn http(runtime: &RedDBRuntime, method: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = reddb::server::RedDBServer::new(runtime.clone());
    let worker = std::thread::spawn(move || server.serve_one_on(listener));
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    worker.join().unwrap().unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

#[test]
fn http_contract_round_trip_enforces_writes_and_single_type_payloads() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH people").unwrap();
    let route = "/v1/collections/people/graph/contract";
    assert_eq!(http(&runtime, "GET", route, "").1, serde_json::Value::Null);
    assert_eq!(
        http(&runtime, "PUT", route, &contract().to_json().unwrap()).0,
        200
    );
    assert_eq!(
        http(&runtime, "GET", route, "").1,
        serde_json::from_str::<serde_json::Value>(&contract().to_json().unwrap()).unwrap()
    );
    assert!(
        http(
            &runtime,
            "POST",
            "/collections/people/nodes",
            r#"{"label":"Person","node_type":["Person","Company"],"properties":{"age":30}}"#
        )
        .0 >= 400
    );
    assert!(
        http(
            &runtime,
            "POST",
            "/collections/people/nodes",
            r#"{"label":"alice","node_type":"Person"}"#
        )
        .0 >= 400
    );
    assert_eq!(
        http(
            &runtime,
            "POST",
            "/collections/people/nodes",
            r#"{"label":"alice","node_type":"Person","properties":{"age":30}}"#
        )
        .0,
        200
    );
    runtime
        .execute_query("INSERT INTO people NODE (label,node_type) VALUES ('database','Company')")
        .unwrap();
    runtime
        .execute_query(
            "INSERT INTO people EDGE (label,from,to) VALUES ('WORKS_AT','alice','database')",
        )
        .unwrap();
    let (status, result) = http(
        &runtime,
        "POST",
        "/hybrid/search",
        r#"{"collections":["people"],"query":"database","graph":{"edge_labels":["WORKS_AT"],"node_type":"Person"},"limit":10}"#,
    );
    assert_eq!(status, 200, "{result}");
    assert!(
        result.to_string().contains("alice"),
        "relation-only match: {result}"
    );
    assert!(
        result.to_string().contains("hop_distance"),
        "relation explanation: {result}"
    );
    assert_eq!(http(&runtime, "PUT", route, "null").0, 200);
    assert!(runtime.graph_contract("people").unwrap().is_none());
    clear_current_connection_id();
}
