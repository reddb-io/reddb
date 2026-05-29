//! End-to-end test for the `components(<graph>)` table-valued function (#795).
//!
//! Creates two disconnected subgraphs in a single graph store via the real
//! supported SQL INSERT path (the same `INSERT INTO <c> NODE/EDGE` forms used by
//! `integration_graph_ops.rs` / the shared fixtures), runs
//! `SELECT * FROM components(g)` through the runtime query path, and asserts the
//! result groups nodes into exactly two islands with correct membership and
//! deterministic results across repeated runs.
//!
//! Known v0 limitation: the `components(<collection>)` argument is NOT resolved
//! to a named collection. The TVF runs over the WHOLE graph store regardless of
//! the argument value (it materializes the full graph). Scoping the algorithm to
//! a named collection is a follow-up. This test therefore places both subgraphs
//! in one graph collection and expects them to be discovered as two components.

mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::{BTreeMap, BTreeSet};
use support::PersistentDbPath;

/// Run the components TVF and return a map of node_id -> island_id.
fn run_components(rt: &RedDBRuntime, graph: &str) -> BTreeMap<String, i64> {
    let sql = format!("SELECT * FROM components({graph})");
    let result = rt.execute_query(&sql).expect("components query");
    assert_eq!(
        result.engine, "runtime-graph-tvf",
        "components(...) must route through the TVF executor"
    );
    let unified = result.result;

    // SELECT * must return both columns in order.
    assert_eq!(
        unified.columns,
        vec!["node_id".to_string(), "island_id".to_string()],
        "columns must be node_id, island_id"
    );

    let mut map = BTreeMap::new();
    for rec in &unified.records {
        let node = match rec.get("node_id").expect("node_id present") {
            Value::Text(s) => s.to_string(),
            other => panic!("node_id should be Text, got {other:?}"),
        };
        let island = match rec.get("island_id").expect("island_id present") {
            Value::Integer(n) => *n,
            other => panic!("island_id should be Integer, got {other:?}"),
        };
        map.insert(node, island);
    }
    map
}

#[test]
fn components_tvf_two_disconnected_subgraphs() {
    let db = PersistentDbPath::new("issue_795_components_tvf");
    let rt = db.open_runtime();

    // Two disconnected subgraphs in one graph collection:
    //   subgraph A: 1 - 2 - 3 mutually connected (chain)
    //   subgraph B: 4 - 5 connected
    let n1 = make_node(&rt, "g", "1");
    let n2 = make_node(&rt, "g", "2");
    let n3 = make_node(&rt, "g", "3");
    let n4 = make_node(&rt, "g", "4");
    let n5 = make_node(&rt, "g", "5");

    make_edge(&rt, "g", &n1, &n2);
    make_edge(&rt, "g", &n2, &n3);
    make_edge(&rt, "g", &n4, &n5);

    let map = run_components(&rt, "g");

    // Exactly 5 nodes assigned.
    assert_eq!(map.len(), 5, "all nodes assigned: {map:?}");

    // Exactly two distinct island ids.
    let islands: BTreeSet<i64> = map.values().copied().collect();
    assert_eq!(islands.len(), 2, "expected two islands, got {islands:?}");

    // Membership grouping: {1,2,3} share an island; {4,5} share another;
    // the two islands differ.
    assert_eq!(map[&n1], map[&n2]);
    assert_eq!(map[&n2], map[&n3]);
    assert_eq!(map[&n4], map[&n5]);
    assert_ne!(map[&n1], map[&n4]);

    // Determinism: a second run yields identical results.
    let map2 = run_components(&rt, "g");
    assert_eq!(map, map2, "components must be deterministic across runs");
}

/// Insert a graph node via SQL and return its assigned graph node id.
fn make_node(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    rt.execute_query(&format!(
        "INSERT INTO {collection} NODE (label, node_type) VALUES ('{label}', 'Host')"
    ))
    .unwrap_or_else(|err| panic!("insert node {label}: {err:?}"));
    graph_node_id(rt, collection, label)
}

/// Insert a directed edge between two graph nodes via SQL.
fn make_edge(rt: &RedDBRuntime, collection: &str, from: &str, to: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} EDGE (label, from, to, weight) VALUES ('connects', {from}, {to}, 1.0)"
    ))
    .unwrap_or_else(|err| panic!("insert edge {from}->{to}: {err:?}"));
}

/// Resolve a graph node's storage id by its label.
fn graph_node_id(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    use reddb::storage::EntityKind;
    rt.db()
        .store()
        .get_collection(collection)
        .unwrap_or_else(|| panic!("collection '{collection}' should exist"))
        .query_all(|_| true)
        .into_iter()
        .find_map(|entity| match &entity.kind {
            EntityKind::GraphNode(node) if node.label == label => Some(entity.id.raw().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("graph node '{label}' not found in '{collection}'"))
}
