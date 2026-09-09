use reddb::application::VcsUseCases;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::EntityKind;
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn execute(runtime: &RedDBRuntime, query: &str) {
    runtime
        .execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
}

fn fixture(runtime: &RedDBRuntime) -> u64 {
    for query in [
        "SET CONFIG runtime.result_cache.enabled = false",
        "INSERT INTO links NODE (label, score) VALUES ('alice', 0)",
        "INSERT INTO links NODE (label, score) VALUES ('bob', 1)",
        "INSERT INTO links EDGE (label, from_rid, to_rid, visible) VALUES ('step', 'alice', 'bob', true)",
        "CREATE FUNCTION path_scores() RETURNS TABLE (score INT) EFFECT READ AS 'MATCH (a)-[:step]->(b) RETURN b.score AS score'",
    ] { execute(runtime, query); }
    VcsUseCases::new(runtime)
        .set_versioned("links", true)
        .expect("versioning");
    physical_bob(runtime)
}

fn physical_bob(runtime: &RedDBRuntime) -> u64 {
    let nodes = runtime
        .db()
        .store()
        .get_collection("links")
        .expect("links")
        .query_all(|entity| {
            matches!(&entity.kind, EntityKind::GraphNode(node) if node.label == "bob")
                && entity.xmax == 0
        });
    assert_eq!(nodes.len(), 1);
    nodes[0].id.raw()
}

fn assert_path(runtime: &RedDBRuntime, edge: &str, score: Option<i64>, rid: u64) {
    let query = format!(
        "MATCH (a)-[e:{edge}]->(b) RETURN b.score AS score, b.id AS rid, e.visible AS visible"
    );
    let result = runtime.execute_query(&query).expect("path");
    assert_eq!(
        result.result.records.len(),
        usize::from(score.is_some()),
        "{query}: {:?}",
        result.result.records
    );
    if let Some(score) = score {
        let row = &result.result.records[0];
        assert_eq!(row.get("score"), Some(&Value::Integer(score)));
        assert_eq!(row.get("rid"), Some(&Value::text(rid.to_string())));
        assert_eq!(row.get("visible"), Some(&Value::Boolean(true)));
    }
}

#[test]
fn versioned_paths_survive_savepoint_commit_delete_and_rollback() {
    for sealed in [false, true] {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let rid = fixture(&runtime);
        if sealed {
            runtime
                .db()
                .store()
                .get_collection("links")
                .expect("links")
                .force_seal()
                .expect("seal");
        }
        execute(&runtime, "BEGIN");
        execute(
            &runtime,
            "UPDATE links NODES SET score = 2 WHERE label = 'bob'",
        );
        assert_ne!(physical_bob(&runtime), rid);
        assert_path(&runtime, "step", Some(2), rid);
        execute(&runtime, "SAVEPOINT changed");
        execute(
            &runtime,
            "UPDATE links NODES SET score = 3 WHERE label = 'bob'",
        );
        assert_path(&runtime, "step", Some(3), rid);
        execute(&runtime, "ROLLBACK TO SAVEPOINT changed");
        assert_path(&runtime, "step", Some(2), rid);
        execute(
            &runtime,
            "UPDATE links EDGES SET weight = 2 WHERE label = 'step'",
        );
        execute(&runtime, "COMMIT");
        assert_path(&runtime, "step", Some(2), rid);
        let call = runtime.execute_query("CALL path_scores()").expect("CALL");
        assert_eq!(call.result.records.len(), 1);
        assert_eq!(
            call.result.records[0].get("score"),
            Some(&Value::Integer(2))
        );
        execute(&runtime, "BEGIN");
        execute(&runtime, "DELETE FROM links WHERE label = 'bob'");
        assert_path(&runtime, "step", None, rid);
        execute(&runtime, "ROLLBACK");
        assert_path(&runtime, "step", Some(2), rid);
    }
}

#[test]
fn versioned_paths_resolve_intermediate_endpoints_after_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph.rdb");
    let rid;
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        rid = fixture(&runtime);
        execute(
            &runtime,
            "UPDATE links NODES SET score = 2 WHERE label = 'bob'",
        );
        let intermediate = physical_bob(&runtime);
        assert_ne!(intermediate, rid);
        // Numeric endpoints preserve compatibility with previously persisted physical references.
        execute(&runtime, &format!("INSERT INTO archived_edges EDGE (label, from_rid, to_rid, visible) VALUES ('legacy', {rid}, {intermediate}, true)"));
        execute(&runtime, "INSERT INTO links EDGE (label, from_rid, to_rid, visible) VALUES ('later', 'alice', 'bob', true)");
        execute(
            &runtime,
            "UPDATE links NODES SET score = 3 WHERE label = 'bob'",
        );
        for edge in ["step", "legacy", "later"] {
            assert_path(&runtime, edge, Some(3), rid);
        }
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    execute(&runtime, "SET CONFIG runtime.result_cache.enabled = false");
    for edge in ["step", "legacy", "later"] {
        assert_path(&runtime, edge, Some(3), rid);
    }
    execute(&runtime, "INSERT INTO links EDGE (label, from_rid, to_rid, visible) VALUES ('reopened', 'alice', 'bob', true)");
    assert_path(&runtime, "reopened", Some(3), rid);
    execute(
        &runtime,
        "INSERT INTO links NODE (label, score) VALUES ('bob', 9)",
    );
    let error = runtime
        .execute_query(
            "INSERT INTO links EDGE (label, from_rid, to_rid) VALUES ('ambiguous', 'alice', 'bob')",
        )
        .expect_err("distinct nodes remain ambiguous");
    assert!(error.to_string().contains("ambiguous label"), "{error}");
}

#[test]
fn versioned_paths_keep_reader_snapshot_during_concurrent_commit() {
    struct ConnectionScope;
    impl Drop for ConnectionScope {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _scope = ConnectionScope;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(73001);
    let rid = fixture(&runtime);
    execute(&runtime, "BEGIN");
    assert_path(&runtime, "step", Some(1), rid);
    set_current_connection_id(73002);
    execute(&runtime, "BEGIN");
    execute(
        &runtime,
        "UPDATE links NODES SET score = 2 WHERE label = 'bob'",
    );
    assert_path(&runtime, "step", Some(2), rid);
    set_current_connection_id(73001);
    assert_path(&runtime, "step", Some(1), rid);
    set_current_connection_id(73002);
    execute(&runtime, "COMMIT");
    set_current_connection_id(73001);
    assert_path(&runtime, "step", Some(1), rid);
    execute(&runtime, "COMMIT");
    assert_path(&runtime, "step", Some(2), rid);
}

#[test]
fn versioned_endpoint_aliases_do_not_bypass_node_or_edge_rls() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let rid = fixture(&runtime);
    execute(
        &runtime,
        "UPDATE links NODES SET score = 2 WHERE label = 'bob'",
    );
    let intermediate = physical_bob(&runtime);
    execute(&runtime, &format!("INSERT INTO links EDGE (label, from_rid, to_rid, visible) VALUES ('legacy', {rid}, {intermediate}, true)"));
    execute(
        &runtime,
        "UPDATE links NODES SET score = 3 WHERE label = 'bob'",
    );
    for query in [
        "CREATE POLICY nodes ON NODES OF links USING (properties.score < 3)",
        "CREATE POLICY edges ON EDGES OF links USING (properties.visible = true)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
    ] {
        execute(&runtime, query);
    }
    assert_path(&runtime, "legacy", None, rid);
    execute(&runtime, "DROP POLICY nodes ON links");
    execute(
        &runtime,
        "CREATE POLICY nodes ON NODES OF links USING (true)",
    );
    assert_path(&runtime, "legacy", Some(3), rid);
    execute(&runtime, "DROP POLICY edges ON links");
    execute(
        &runtime,
        "CREATE POLICY edges ON EDGES OF links USING (false)",
    );
    assert_path(&runtime, "legacy", None, rid);
}
