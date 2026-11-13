use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::runtime::{RuntimeFilter, RuntimeFilterValue, RuntimeGraphPattern, RuntimeQueryWeights};
use reddb::storage::EntityKind;
use reddb::storage::QueryResult;
use reddb::RedDBRuntime;

const CONNECTION: u64 = 9601;

fn fixture(runtime: &RedDBRuntime) {
    set_current_connection_id(CONNECTION);
    runtime.execute_query("CREATE GRAPH links").unwrap();
    runtime.vcs_set_versioned("links", true).unwrap();
    let seed = runtime
        .db()
        .node("links", "anchor")
        .node_type("Topic")
        .property("text", "database")
        .embedding("semantic", vec![1.0, 0.0])
        .save()
        .unwrap();
    let target = runtime
        .db()
        .node("links", "bob")
        .node_type("Person")
        .property("rank", 2i64)
        .save()
        .unwrap();
    let far = runtime
        .db()
        .node("links", "carol")
        .node_type("Person")
        .property("rank", 3i64)
        .save()
        .unwrap();
    runtime
        .db()
        .edge("links", "ABOUT")
        .from(seed)
        .to(target)
        .save()
        .unwrap();
    runtime
        .db()
        .edge("links", "ABOUT")
        .from(target)
        .to(far)
        .save()
        .unwrap();
    // Parallel edges and reverse cycles must not duplicate or inflate a result.
    runtime
        .db()
        .edge("links", "ABOUT")
        .from(target)
        .to(seed)
        .save()
        .unwrap();
}

fn search(
    runtime: &RedDBRuntime,
    vector: bool,
    label: &str,
    filters: Vec<RuntimeFilter>,
    min: f32,
) -> QueryResult {
    runtime
        .search_hybrid(
            vector.then(|| vec![1.0, 0.0]),
            (!vector).then(|| "database".into()),
            Some(10),
            Some(vec!["links".into()]),
            None,
            None,
            Some(RuntimeGraphPattern {
                node_label: None,
                node_type: Some("Person".into()),
                edge_labels: vec![label.into()],
            }),
            filters,
            Some(RuntimeQueryWeights {
                vector: 0.5,
                graph: 0.3,
                filter: 0.2,
            }),
            Some(min),
            Some(10),
        )
        .unwrap()
}

fn labels(result: &QueryResult) -> Vec<String> {
    result
        .matches
        .iter()
        .map(|item| match &item.entity.kind {
            EntityKind::GraphNode(node) => node.label.clone(),
            _ => panic!("expected node"),
        })
        .collect()
}

#[test]
fn text_and_vector_seeds_expand_only_requested_relations_once() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    fixture(&runtime);
    for vector in [false, true] {
        let result = search(&runtime, vector, "ABOUT", vec![], 0.0);
        assert_eq!(labels(&result), ["bob"]);
        let item = &result.matches[0];
        assert!((item.score - 0.3).abs() < 0.0001);
        assert_eq!(item.components.graph_match, Some(1.0));
        assert_eq!(item.components.hop_distance, Some(1));
        assert_eq!(item.path.as_ref().unwrap().len(), 3);
        assert!(search(&runtime, vector, "UNRELATED", vec![], 0.0)
            .matches
            .is_empty());
        assert!(search(&runtime, vector, "ABOUT", vec![], 0.4)
            .matches
            .is_empty());
        let filter = RuntimeFilter {
            field: "rank".into(),
            op: "gt".into(),
            value: Some(RuntimeFilterValue::Int(2)),
        };
        assert!(search(&runtime, vector, "ABOUT", vec![filter], 0.0)
            .matches
            .is_empty());
    }
    clear_current_connection_id();
}

#[test]
fn relation_search_authorizes_both_endpoints_and_edges_before_expansion() {
    for hide_nodes in [false, true] {
        let runtime = RedDBRuntime::in_memory().unwrap();
        fixture(&runtime);
        let node_policy = if hide_nodes {
            "CREATE POLICY nodes ON NODES OF links USING (node_type = 'Topic')"
        } else {
            "CREATE POLICY nodes ON NODES OF links USING (true)"
        };
        let edge_policy = if hide_nodes {
            "CREATE POLICY edges ON EDGES OF links USING (true)"
        } else {
            "CREATE POLICY edges ON EDGES OF links USING (false)"
        };
        runtime.execute_query(node_policy).unwrap();
        runtime.execute_query(edge_policy).unwrap();
        runtime
            .execute_query("ALTER TABLE links ENABLE ROW LEVEL SECURITY")
            .unwrap();
        for vector in [false, true] {
            assert!(search(&runtime, vector, "ABOUT", vec![], 0.0)
                .matches
                .is_empty());
        }
        clear_current_connection_id();
    }
}

#[test]
fn relation_search_keeps_branch_and_transaction_snapshots() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    fixture(&runtime);
    runtime
        .vcs_commit(CreateCommitInput {
            connection_id: CONNECTION,
            message: "base".into(),
            author: Author {
                name: "test".into(),
                email: "test@reddb.io".into(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap();
    runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: CONNECTION,
            name: "agent".into(),
            from: None,
        })
        .unwrap();
    runtime
        .vcs_checkout(CheckoutInput {
            connection_id: CONNECTION,
            target: CheckoutTarget::Branch("agent".into()),
            force: false,
        })
        .unwrap();
    runtime
        .execute_query("DELETE FROM links WHERE label='ABOUT'")
        .unwrap();
    assert!(search(&runtime, false, "ABOUT", vec![], 0.0)
        .matches
        .is_empty());
    runtime
        .vcs_checkout(CheckoutInput {
            connection_id: CONNECTION,
            target: CheckoutTarget::Branch("main".into()),
            force: false,
        })
        .unwrap();
    assert_eq!(
        labels(&search(&runtime, false, "ABOUT", vec![], 0.0)),
        ["bob"]
    );
    runtime.execute_query("BEGIN").unwrap();
    set_current_connection_id(CONNECTION + 1);
    runtime.execute_query("BEGIN").unwrap();
    runtime
        .execute_query("DELETE FROM links WHERE label='ABOUT'")
        .unwrap();
    assert!(search(&runtime, false, "ABOUT", vec![], 0.0)
        .matches
        .is_empty());
    set_current_connection_id(CONNECTION);
    assert_eq!(
        labels(&search(&runtime, false, "ABOUT", vec![], 0.0)),
        ["bob"]
    );
    set_current_connection_id(CONNECTION + 1);
    runtime.execute_query("COMMIT").unwrap();
    set_current_connection_id(CONNECTION);
    assert_eq!(
        labels(&search(&runtime, false, "ABOUT", vec![], 0.0)),
        ["bob"]
    );
    runtime.execute_query("COMMIT").unwrap();
    assert!(search(&runtime, false, "ABOUT", vec![], 0.0)
        .matches
        .is_empty());
    clear_current_connection_id();
}
