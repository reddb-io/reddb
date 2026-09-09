use reddb::application::VcsUseCases;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::runtime::{RuntimeGraphDirection, RuntimeGraphProjection};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn execute(runtime: &RedDBRuntime, query: &str) {
    runtime
        .execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
}

fn fixture(runtime: &RedDBRuntime) -> String {
    for query in [
        "SET CONFIG runtime.result_cache.enabled = false",
        "INSERT INTO links NODE (label,node_type,score) VALUES ('alice','Person',0)",
        "INSERT INTO links NODE (label,node_type,score) VALUES ('bob','Person',1)",
        "INSERT INTO links EDGE (label,from,to,visible) VALUES ('step','alice','bob',true)",
    ] {
        execute(runtime, query);
    }
    VcsUseCases::new(runtime)
        .set_versioned("links", true)
        .expect("versioning");
    let result = runtime
        .execute_query("GRAPH PROPERTIES 'bob'")
        .expect("properties");
    result.result.records[0]
        .get("node_id")
        .expect("node id")
        .as_text()
        .expect("text id")
        .to_string()
}

fn labels(runtime: &RedDBRuntime, query: &str) -> Vec<String> {
    let result = runtime
        .execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
    let mut labels: Vec<_> = result
        .result
        .records
        .iter()
        .map(|row| {
            row.get("label")
                .expect("label")
                .as_text()
                .expect("text label")
                .to_string()
        })
        .collect();
    labels.sort();
    labels
}

fn assert_native(runtime: &RedDBRuntime, rid: &str, score: i64, node_type: &str) {
    for query in [
        "GRAPH NEIGHBORHOOD 'alice' DEPTH 2",
        "GRAPH TRAVERSE FROM 'alice' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 2",
    ] {
        assert_eq!(labels(runtime, query), ["alice", "bob"], "{query}");
    }
    let path = runtime
        .execute_query(&format!(
            "GRAPH SHORTEST_PATH 'alice' TO '{rid}' ALGORITHM dijkstra"
        ))
        .expect("stable target");
    assert_eq!(
        path.result.records[0].get("path_found"),
        Some(&Value::Boolean(true))
    );
    assert_eq!(
        path.result.records[0].get("hop_count"),
        Some(&Value::Integer(1))
    );
    let props = runtime
        .execute_query("GRAPH PROPERTIES 'bob'")
        .expect("visible properties");
    assert_eq!(props.result.records.len(), 1);
    assert_eq!(
        props.result.records[0].get("node_id"),
        Some(&Value::text(rid))
    );
    assert_eq!(
        props.result.records[0].get("score"),
        Some(&Value::Integer(score))
    );
    assert_eq!(
        props.result.records[0].get("node_type"),
        Some(&Value::text(node_type))
    );
    let components = runtime
        .execute_query("SELECT * FROM components(links)")
        .expect("components");
    assert_eq!(components.result.records.len(), 2);
    assert_eq!(
        components.result.records[0].get("island_id"),
        components.result.records[1].get("island_id")
    );
    assert!(components
        .result
        .records
        .iter()
        .any(|row| row.get("node_id") == Some(&Value::text(rid))));
}

#[test]
fn native_graph_paths_and_properties_survive_versioning_and_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph.rdb");
    let rid;
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        rid = fixture(&runtime);
        runtime
            .db()
            .store()
            .get_collection("links")
            .expect("links")
            .force_seal()
            .expect("seal");
        execute(&runtime, "BEGIN");
        execute(
            &runtime,
            "UPDATE links NODES SET score=2,node_type='UpdatedPerson' WHERE label='bob'",
        );
        assert_native(&runtime, &rid, 2, "UpdatedPerson");
        execute(&runtime, "SAVEPOINT changed");
        execute(&runtime, "UPDATE links NODES SET score=3 WHERE label='bob'");
        assert_native(&runtime, &rid, 3, "UpdatedPerson");
        execute(&runtime, "ROLLBACK TO SAVEPOINT changed");
        assert_native(&runtime, &rid, 2, "UpdatedPerson");
        execute(&runtime, "COMMIT");
        assert_native(&runtime, &rid, 2, "UpdatedPerson");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    execute(&runtime, "SET CONFIG runtime.result_cache.enabled = false");
    assert_native(&runtime, &rid, 2, "UpdatedPerson");
    execute(&runtime, "BEGIN");
    execute(&runtime, "DELETE FROM links WHERE label='bob'");
    assert_eq!(
        labels(&runtime, "GRAPH NEIGHBORHOOD 'alice' DEPTH 2"),
        ["alice"]
    );
    assert!(runtime.execute_query("GRAPH PROPERTIES 'bob'").is_err());
    execute(&runtime, "ROLLBACK");
    assert_native(&runtime, &rid, 2, "UpdatedPerson");
}

#[test]
fn native_graph_rls_matches_pattern_visibility() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let rid = fixture(&runtime);
    execute(&runtime, "UPDATE links NODES SET score=2 WHERE label='bob'");
    for query in [
        "CREATE POLICY nodes ON NODES OF links USING (properties.score < 2)",
        "CREATE POLICY edges ON EDGES OF links USING (properties.visible = true)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
    ] {
        execute(&runtime, query);
    }
    assert_eq!(
        labels(&runtime, "GRAPH NEIGHBORHOOD 'alice' DEPTH 2"),
        ["alice"]
    );
    assert!(
        runtime.execute_query("GRAPH PROPERTIES 'bob'").is_err(),
        "denied properties"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM components(links)")
            .expect("components")
            .result
            .records
            .len(),
        1
    );
    execute(&runtime, "DROP POLICY nodes ON links");
    execute(
        &runtime,
        "CREATE POLICY nodes ON NODES OF links USING (true)",
    );
    assert_native(&runtime, &rid, 2, "Person");
    execute(&runtime, "DROP POLICY edges ON links");
    execute(
        &runtime,
        "CREATE POLICY edges ON EDGES OF links USING (false)",
    );
    assert_eq!(
        labels(&runtime, "GRAPH NEIGHBORHOOD 'alice' DEPTH 2"),
        ["alice"]
    );
}

#[test]
fn native_graph_api_installs_snapshot_and_preserves_projection() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let rid = fixture(&runtime);
    execute(
        &runtime,
        "UPDATE links NODES SET node_type='UpdatedPerson',score=2 WHERE label='bob'",
    );
    let neighborhood = runtime
        .graph_neighborhood("alice", RuntimeGraphDirection::Outgoing, 2, None, None)
        .expect("native API");
    assert_eq!(neighborhood.nodes.len(), 2, "one visible version per node");
    assert!(neighborhood.nodes.iter().any(|node| node.node.id == rid));
    let projection = RuntimeGraphProjection {
        node_labels: Some(vec!["alice".into(), "bob".into()]),
        node_types: Some(vec!["Person".into()]),
        edge_labels: Some(vec!["step".into()]),
    };
    let projected = runtime
        .graph_neighborhood(
            "alice",
            RuntimeGraphDirection::Outgoing,
            2,
            None,
            Some(projection),
        )
        .expect("projection");
    assert_eq!(
        projected.nodes.len(),
        1,
        "updated node no longer matches type projection"
    );
    assert!(projected.edges.is_empty());
    let projection = RuntimeGraphProjection {
        edge_labels: Some(vec!["other".into()]),
        ..Default::default()
    };
    let projected = runtime
        .graph_neighborhood(
            "alice",
            RuntimeGraphDirection::Outgoing,
            2,
            None,
            Some(projection),
        )
        .expect("edge projection");
    assert_eq!(projected.nodes.len(), 1);
}

#[test]
fn native_graph_uses_transaction_snapshot_for_properties_and_paths() {
    struct ConnectionScope;
    impl Drop for ConnectionScope {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _scope = ConnectionScope;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(74001);
    let rid = fixture(&runtime);
    execute(&runtime, "BEGIN");
    set_current_connection_id(74002);
    execute(&runtime, "BEGIN");
    execute(
        &runtime,
        "UPDATE links NODES SET score=2,node_type='UpdatedPerson' WHERE label='bob'",
    );
    assert_native(&runtime, &rid, 2, "UpdatedPerson");
    execute(&runtime, "COMMIT");
    set_current_connection_id(74001);
    assert_native(&runtime, &rid, 1, "Person");
    let projection = RuntimeGraphProjection {
        node_types: Some(vec!["Person".into()]),
        ..Default::default()
    };
    let direct = runtime
        .graph_neighborhood(
            "alice",
            RuntimeGraphDirection::Outgoing,
            2,
            None,
            Some(projection),
        )
        .expect("direct API transaction snapshot");
    assert_eq!(direct.nodes.len(), 2);
    execute(&runtime, "COMMIT");
    assert_native(&runtime, &rid, 2, "UpdatedPerson");
}

#[test]
fn native_properties_preserve_id_precedence_and_label_ambiguity() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let rid = fixture(&runtime);
    execute(
        &runtime,
        &format!("INSERT INTO links NODE (label,node_type,score) VALUES ('{rid}','Person',9)"),
    );
    let properties = runtime
        .execute_query(&format!("GRAPH PROPERTIES '{rid}'"))
        .expect("ID takes precedence over a numeric label");
    assert_eq!(
        properties.result.records[0].get("label"),
        Some(&Value::text("bob"))
    );
    assert_eq!(
        properties.result.records[0].get("score"),
        Some(&Value::Integer(1))
    );
    execute(
        &runtime,
        "INSERT INTO links NODE (label,node_type,score) VALUES ('bob','Person',8)",
    );
    let error = runtime
        .execute_query("GRAPH PROPERTIES 'bob'")
        .expect_err("ambiguous label");
    assert!(
        error.to_string().contains("ambiguous graph node reference"),
        "{error}"
    );
}
