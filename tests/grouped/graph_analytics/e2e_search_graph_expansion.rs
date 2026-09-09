use reddb::application::{SearchContextInput, VcsUseCases};
use reddb::runtime::{ContextSearchResult, DiscoveryMethod};
use reddb::storage::{EntityData, EntityKind};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn execute(runtime: &RedDBRuntime, query: &str) {
    runtime
        .execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
}

fn input(query: &str) -> SearchContextInput {
    SearchContextInput {
        query: query.into(),
        field: None,
        vector: None,
        collections: None,
        limit: Some(50),
        graph_depth: Some(3),
        graph_max_edges: Some(20),
        max_cross_refs: None,
        follow_cross_refs: Some(false),
        expand_graph: Some(true),
        global_scan: Some(true),
        reindex: Some(false),
        min_score: Some(0.0),
    }
}

fn fixture(runtime: &RedDBRuntime) {
    for query in [
        "SET CONFIG runtime.result_cache.enabled = false",
        "INSERT INTO links NODE (label,node_type,score) VALUES ('alice','Person',0)",
        "INSERT INTO links NODE (label,node_type,score) VALUES ('bob','Person',1)",
        "INSERT INTO links NODE (label,node_type,score) VALUES ('carol','Person',0)",
        "INSERT INTO links EDGE (label,from,to,visible) VALUES ('step','alice','bob',true)",
        "INSERT INTO links EDGE (label,from,to,visible) VALUES ('step','bob','carol',true)",
    ] {
        execute(runtime, query);
    }
    VcsUseCases::new(runtime)
        .set_versioned("links", true)
        .expect("versioning");
}

fn search(runtime: &RedDBRuntime) -> ContextSearchResult {
    runtime
        .search_context_input(input("alice"))
        .expect("search")
}

fn labels(result: &ContextSearchResult) -> Vec<String> {
    let mut labels: Vec<_> = result
        .graph
        .nodes
        .iter()
        .map(|item| match &item.entity.kind {
            EntityKind::GraphNode(node) => node.label.clone(),
            _ => panic!("node expected"),
        })
        .collect();
    labels.sort();
    labels
}

fn assert_score(result: &ContextSearchResult, score: i64) {
    let bob = result
        .graph
        .nodes
        .iter()
        .find(
            |item| matches!(&item.entity.kind, EntityKind::GraphNode(node) if node.label == "bob"),
        )
        .expect("bob reachable");
    let EntityData::Node(node) = &bob.entity.data else {
        panic!("node")
    };
    assert_eq!(node.properties.get("score"), Some(&Value::Integer(score)));
    assert!(matches!(
        bob.discovery,
        DiscoveryMethod::GraphTraversal { depth: 1, .. }
    ));
    assert_eq!(labels(result), ["alice", "bob", "carol"]);
}

#[test]
fn search_graph_hydrates_visible_versions_and_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        fixture(&runtime);
        execute(&runtime, "BEGIN");
        execute(&runtime, "UPDATE links NODES SET score=2 WHERE label='bob'");
        assert_score(&search(&runtime), 2);
        execute(&runtime, "SAVEPOINT changed");
        execute(&runtime, "UPDATE links NODES SET score=3 WHERE label='bob'");
        assert_score(&search(&runtime), 3);
        execute(&runtime, "ROLLBACK TO SAVEPOINT changed");
        assert_score(&search(&runtime), 2);
        execute(&runtime, "COMMIT");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    assert_score(&search(&runtime), 2);
}

#[test]
fn search_graph_denied_node_cannot_bridge_to_visible_node() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    for query in [
        "CREATE POLICY nodes ON NODES OF links USING (properties.score = 0)",
        "CREATE POLICY edges ON EDGES OF links USING (true)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
    ] {
        execute(&runtime, query);
    }
    assert_eq!(labels(&search(&runtime)), ["alice"]);
}

#[test]
fn search_graph_denied_edge_cannot_bridge() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    for query in [
        "CREATE POLICY nodes ON NODES OF links USING (true)",
        "CREATE POLICY edges ON EDGES OF links USING (false)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
    ] {
        execute(&runtime, query);
    }
    assert_eq!(labels(&search(&runtime)), ["alice"]);
}

#[test]
fn search_graph_keeps_reader_snapshot_and_hides_deleted_bridges() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    struct ConnectionScope;
    impl Drop for ConnectionScope {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _scope = ConnectionScope;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(75001);
    fixture(&runtime);
    execute(&runtime, "BEGIN");
    set_current_connection_id(75002);
    execute(&runtime, "BEGIN");
    execute(&runtime, "UPDATE links NODES SET score=2 WHERE label='bob'");
    execute(
        &runtime,
        "UPDATE links NODES SET score=5 WHERE label='alice'",
    );
    assert_score(&search(&runtime), 2);
    set_current_connection_id(75001);
    assert_score(&search(&runtime), 1);
    set_current_connection_id(75002);
    execute(&runtime, "COMMIT");
    set_current_connection_id(75001);
    assert_score(&search(&runtime), 1);
    execute(&runtime, "COMMIT");
    assert_score(&search(&runtime), 2);
    execute(&runtime, "BEGIN");
    execute(&runtime, "DELETE FROM links WHERE label='bob'");
    assert_eq!(labels(&search(&runtime)), ["alice"]);
    execute(&runtime, "ROLLBACK");
    assert_score(&search(&runtime), 2);
}

#[test]
fn search_graph_preserves_destination_collection_and_scope() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    execute(
        &runtime,
        "INSERT INTO people NODE (label,node_type) VALUES ('dave','Person')",
    );
    let dave = runtime
        .search_context_input(input("dave"))
        .expect("dave")
        .graph
        .nodes[0]
        .entity
        .id
        .raw();
    let alice = search(&runtime).graph.nodes.iter().find(|item| matches!(&item.entity.kind, EntityKind::GraphNode(node) if node.label == "alice")).expect("alice").entity.id.raw();
    execute(
        &runtime,
        &format!(
            "INSERT INTO links EDGE (label,from_rid,to_rid) VALUES ('outside',{alice},{dave})"
        ),
    );
    let result = search(&runtime);
    assert_eq!(labels(&result), ["alice", "bob", "carol", "dave"]);
    assert_eq!(
        result
            .graph
            .nodes
            .iter()
            .find(|item| item.entity.id.raw() == dave)
            .expect("destination")
            .collection,
        "people"
    );
    let mut scoped = input("alice");
    scoped.collections = Some(vec!["links".into()]);
    assert_eq!(
        labels(&runtime.search_context_input(scoped).expect("scoped")),
        ["alice", "bob", "carol"]
    );
    execute(&runtime, "ALTER TABLE people ENABLE ROW LEVEL SECURITY");
    assert_eq!(
        labels(&search(&runtime)),
        ["alice", "bob", "carol"],
        "no policy is deny-default"
    );
}

#[test]
fn search_graph_depth_edges_direction_and_score_are_bounded() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    for depth in [0, 1, 2, 100] {
        let mut request = input("alice");
        request.graph_depth = Some(depth);
        let result = runtime.search_context_input(request).expect("depth");
        let expected = match depth {
            0 => vec!["alice"],
            1 => vec!["alice", "bob"],
            _ => vec!["alice", "bob", "carol"],
        };
        assert_eq!(labels(&result), expected);
    }
    let mut request = input("alice");
    request.graph_max_edges = Some(0);
    assert_eq!(
        labels(&runtime.search_context_input(request).expect("disabled")),
        ["alice"]
    );
    let mut request = input("alice");
    request.min_score = Some(0.6);
    assert_eq!(
        labels(&runtime.search_context_input(request).expect("score")),
        ["alice", "bob"]
    );
    assert_eq!(
        labels(
            &runtime
                .search_context_input(input("carol"))
                .expect("incoming")
        ),
        ["alice", "bob", "carol"]
    );
    execute(
        &runtime,
        "INSERT INTO links EDGE (label,from,to) VALUES ('cycle','alice','carol')",
    );
    let mut request = input("alice");
    request.graph_depth = Some(1);
    request.graph_max_edges = Some(1);
    let expected = labels(
        &runtime
            .search_context_input(request.clone())
            .expect("limited"),
    );
    assert_eq!(expected.len(), 2);
    for _ in 0..5 {
        assert_eq!(
            labels(
                &runtime
                    .search_context_input(request.clone())
                    .expect("repeat")
            ),
            expected
        );
    }
}

#[test]
fn search_graph_resolves_edges_to_retained_intermediate_versions() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    execute(&runtime, "UPDATE links NODES SET score=2 WHERE label='bob'");
    let bob = runtime
        .search_context_input(input("bob"))
        .expect("bob")
        .graph
        .nodes
        .into_iter()
        .find(
            |item| matches!(&item.entity.kind, EntityKind::GraphNode(node) if node.label == "bob"),
        )
        .expect("bob")
        .entity;
    execute(
        &runtime,
        "INSERT INTO links NODE (label,node_type) VALUES ('dave','Person')",
    );
    let dave = runtime
        .search_context_input(input("dave"))
        .expect("dave")
        .graph
        .nodes[0]
        .entity
        .id
        .raw();
    execute(
        &runtime,
        &format!(
            "INSERT INTO links EDGE (label,from_rid,to_rid) VALUES ('later',{},{dave})",
            bob.id.raw()
        ),
    );
    execute(&runtime, "UPDATE links NODES SET score=3 WHERE label='bob'");
    let result = runtime
        .search_context_input(input("dave"))
        .expect("retained endpoint");
    assert_eq!(labels(&result), ["alice", "bob", "carol", "dave"]);
    let updated = result
        .graph
        .nodes
        .iter()
        .find(|item| item.entity.logical_id() == bob.logical_id())
        .expect("same logical node");
    assert_ne!(updated.entity.id, bob.id);
    let EntityData::Node(data) = &updated.entity.data else {
        panic!("node")
    };
    assert_eq!(data.properties.get("score"), Some(&Value::Integer(3)));
}

#[test]
fn search_graph_traverses_direct_matches_and_keeps_strongest_score() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    fixture(&runtime);
    execute(
        &runtime,
        "UPDATE links NODES SET title='origin bridge' WHERE label='alice'",
    );
    execute(
        &runtime,
        "UPDATE links NODES SET title='origin' WHERE label='bob'",
    );
    let mut request = input("origin bridge");
    request.min_score = Some(0.4);
    let result = runtime
        .search_context_input(request)
        .expect("direct bridge");
    assert_eq!(labels(&result), ["alice", "bob", "carol"]);
    let carol = result.graph.nodes.iter().find(|item| matches!(&item.entity.kind, EntityKind::GraphNode(node) if node.label == "carol")).expect("carol");
    assert!((carol.score - 0.9 * 0.7 * 0.7).abs() < 0.0001);
    assert!(matches!(
        carol.discovery,
        DiscoveryMethod::GraphTraversal { depth: 2, .. }
    ));
}

#[test]
fn search_graph_edge_versions_follow_reader_snapshot() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    struct ConnectionScope;
    impl Drop for ConnectionScope {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _scope = ConnectionScope;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(75011);
    fixture(&runtime);
    for query in [
        "CREATE POLICY nodes ON NODES OF links USING (true)",
        "CREATE POLICY edges ON EDGES OF links USING (properties.visible = true)",
        "CREATE POLICY edge_update ON links FOR UPDATE USING (true)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
    ] {
        execute(&runtime, query);
    }
    execute(&runtime, "BEGIN");
    set_current_connection_id(75012);
    execute(&runtime, "BEGIN");
    let changed = runtime
        .execute_query("UPDATE links EDGES SET visible=false WHERE label='step'")
        .expect("update edges");
    assert_eq!(
        changed.affected_rows, 2,
        "fixture writer updated both edges"
    );
    assert_eq!(labels(&search(&runtime)), ["alice"]);
    execute(&runtime, "COMMIT");
    set_current_connection_id(75011);
    assert_eq!(labels(&search(&runtime)), ["alice", "bob", "carol"]);
    execute(&runtime, "COMMIT");
    assert_eq!(labels(&search(&runtime)), ["alice"]);
}

#[test]
fn search_graph_cross_reference_seeds_obey_scope_and_rls() {
    use reddb::storage::{CrossRef, RefType, UnifiedEntity};
    use std::collections::HashMap;
    for mode in ["allowed", "scope", "rls", "wrong_collection"] {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        fixture(&runtime);
        let bob = search(&runtime).graph.nodes.into_iter().find(|item| matches!(&item.entity.kind, EntityKind::GraphNode(node) if node.label == "bob")).expect("bob").entity.id;
        let store = runtime.db().store();
        store.get_or_create_collection("sources");
        let id = store.next_entity_id();
        let mut beacon = UnifiedEntity::graph_node(id, "beacon", "Source", HashMap::new());
        beacon.add_cross_ref(CrossRef::new(
            id,
            bob,
            if mode == "wrong_collection" {
                "sources"
            } else {
                "links"
            },
            RefType::RelatedTo,
        ));
        store
            .insert("sources", beacon)
            .expect("cross-reference seed");
        let mut request = input("beacon");
        request.follow_cross_refs = Some(true);
        if mode == "scope" {
            request.collections = Some(vec!["sources".into()]);
        }
        if mode == "rls" {
            execute(&runtime, "ALTER TABLE links ENABLE ROW LEVEL SECURITY");
        }
        let result = runtime
            .search_context_input(request)
            .expect("cross-reference expansion");
        if mode == "allowed" {
            assert_eq!(labels(&result), ["alice", "beacon", "bob", "carol"]);
        } else {
            assert_eq!(labels(&result), ["beacon"], "{mode}");
        }
    }
}
