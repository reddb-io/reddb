//! End-to-end test for the `louvain(<graph> [, resolution => <f64>])`
//! table-valued function (#796).
//!
//! Builds a graph with a clear two-community structure (two densely connected
//! clusters joined by a single thin bridge edge) via the real supported SQL
//! `INSERT INTO <c> NODE/EDGE` path, runs `SELECT * FROM louvain(g)` through the
//! runtime query path, and asserts the result projects `(node_id, community_id)`
//! rows that recover the two clusters, deterministically across reruns. Also
//! exercises the `resolution => <f64>` named-argument form end to end.
//!
//! Known v0 limitation (shared with `components`, #795): the `louvain(<collection>)`
//! argument is NOT resolved to a named collection — the TVF runs over the WHOLE
//! graph store. This test therefore places the whole structure in one graph
//! collection.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::{BTreeMap, BTreeSet};
use support::PersistentDbPath;

/// Run the louvain TVF (optionally with a `resolution` named arg) and return a
/// map of node_id -> community_id.
fn run_louvain(rt: &RedDBRuntime, sql: &str) -> BTreeMap<String, i64> {
    let result = rt.execute_query(sql).expect("louvain query");
    assert_eq!(
        result.engine, "runtime-graph-tvf",
        "louvain(...) must route through the TVF executor"
    );
    let unified = result.result;

    // SELECT * must return both columns in order.
    assert_eq!(
        unified.columns,
        vec!["node_id".to_string(), "community_id".to_string()],
        "columns must be node_id, community_id"
    );

    let mut map = BTreeMap::new();
    for rec in &unified.records {
        let node = match rec.get("node_id").expect("node_id present") {
            Value::Text(s) => s.to_string(),
            other => panic!("node_id should be Text, got {other:?}"),
        };
        let community = match rec.get("community_id").expect("community_id present") {
            Value::Integer(n) => *n,
            other => panic!("community_id should be Integer, got {other:?}"),
        };
        map.insert(node, community);
    }
    map
}

#[test]
fn louvain_tvf_recovers_two_communities() {
    let db = PersistentDbPath::new("issue_796_louvain_tvf");
    let rt = db.open_runtime();

    // Two dense clusters joined by a single thin bridge edge:
    //   cluster A: {1,2,3,4} fully connected
    //   cluster B: {5,6,7,8} fully connected
    //   bridge: 4 - 5
    let ids: Vec<String> = (1..=8)
        .map(|n| make_node(&rt, "g", &n.to_string()))
        .collect();
    let a = &ids[0..4];
    let b = &ids[4..8];
    clique(&rt, "g", a);
    clique(&rt, "g", b);
    make_edge(&rt, "g", &a[3], &b[0]); // thin bridge

    let map = run_louvain(&rt, "SELECT * FROM louvain(g)");

    // All 8 nodes assigned.
    assert_eq!(map.len(), 8, "all nodes assigned: {map:?}");

    // Exactly two communities.
    let communities: BTreeSet<i64> = map.values().copied().collect();
    assert_eq!(
        communities.len(),
        2,
        "expected two communities, got {communities:?}"
    );

    // Cluster A coheres; cluster B coheres; the two differ.
    for n in &a[1..] {
        assert_eq!(map[n], map[&a[0]], "cluster A coheres");
    }
    for n in &b[1..] {
        assert_eq!(map[n], map[&b[0]], "cluster B coheres");
    }
    assert_ne!(map[&a[0]], map[&b[0]], "the two clusters are distinct");

    // Determinism: a second run yields identical results.
    let map2 = run_louvain(&rt, "SELECT * FROM louvain(g)");
    assert_eq!(map, map2, "louvain must be deterministic across runs");
}

#[test]
fn louvain_tvf_accepts_resolution_named_arg() {
    let db = PersistentDbPath::new("issue_796_louvain_resolution");
    let rt = db.open_runtime();

    // Same two-cluster structure as above.
    let ids: Vec<String> = (1..=8)
        .map(|n| make_node(&rt, "g", &n.to_string()))
        .collect();
    let a = &ids[0..4];
    let b = &ids[4..8];
    clique(&rt, "g", a);
    clique(&rt, "g", b);
    make_edge(&rt, "g", &a[3], &b[0]);

    // The full SQL → parser(resolution => f64) → algorithm → projection path.
    let map = run_louvain(&rt, "SELECT * FROM louvain(g, resolution => 1.0)");
    assert_eq!(map.len(), 8);
    let communities: BTreeSet<i64> = map.values().copied().collect();
    assert_eq!(communities.len(), 2, "two communities at resolution 1.0");
    assert_ne!(map[&a[0]], map[&b[0]]);
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

/// Fully connect a set of graph nodes (undirected clique).
fn clique(rt: &RedDBRuntime, collection: &str, nodes: &[String]) {
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            make_edge(rt, collection, &nodes[i], &nodes[j]);
        }
    }
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
