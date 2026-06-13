//! End-to-end tests for the inline `nodes => / edges =>` subquery signature of
//! the graph-analytics table-valued functions (#799).
//!
//! These drive the full SQL → parser → executor path of
//! `SELECT * FROM <tvf>(nodes => (<subquery>), edges => (<subquery>))` over
//! ordinary tables (no graph collection materialized first), for three TVFs —
//! `components`, `louvain`, and `degree_centrality` — plus the wrong-shape
//! error path and the source-collection-scoped result cache.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::{BTreeMap, BTreeSet};
use support::PersistentDbPath;

/// Create the two plain source tables used by the inline form.
fn setup_tables(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE gnodes (id INTEGER)")
        .expect("create gnodes");
    rt.execute_query("CREATE TABLE gedges (src INTEGER, dst INTEGER)")
        .expect("create gedges");
}

fn insert_node(rt: &RedDBRuntime, id: i64) {
    rt.execute_query(&format!("INSERT INTO gnodes (id) VALUES ({id})"))
        .unwrap_or_else(|err| panic!("insert node {id}: {err:?}"));
}

fn insert_edge(rt: &RedDBRuntime, src: i64, dst: i64) {
    rt.execute_query(&format!(
        "INSERT INTO gedges (src, dst) VALUES ({src}, {dst})"
    ))
    .unwrap_or_else(|err| panic!("insert edge {src}->{dst}: {err:?}"));
}

/// Run an inline TVF and return the `(node_id, <int second column>)` map.
fn run_inline(rt: &RedDBRuntime, sql: &str, value_col: &str) -> BTreeMap<String, i64> {
    let result = rt.execute_query(sql).expect("inline tvf query");
    assert_eq!(
        result.engine, "runtime-graph-tvf-inline",
        "inline form must route through the inline TVF executor"
    );
    let unified = result.result;
    let mut map = BTreeMap::new();
    for rec in &unified.records {
        let node = match rec.get("node_id").expect("node_id present") {
            Value::Text(s) => s.to_string(),
            other => panic!("node_id should be Text, got {other:?}"),
        };
        let value = match rec.get(value_col).expect("value column present") {
            Value::Integer(n) => *n,
            other => panic!("{value_col} should be Integer, got {other:?}"),
        };
        map.insert(node, value);
    }
    map
}

#[test]
fn components_inline_over_plain_tables_recovers_two_islands() {
    let db = PersistentDbPath::new("issue_799_components_inline");
    let rt = db.open_runtime();
    setup_tables(&rt);

    // Two disjoint components: {1,2,3} and {4,5,6}.
    for id in 1..=6 {
        insert_node(&rt, id);
    }
    insert_edge(&rt, 1, 2);
    insert_edge(&rt, 2, 3);
    insert_edge(&rt, 4, 5);
    insert_edge(&rt, 5, 6);

    let map = run_inline(
        &rt,
        "SELECT * FROM components(nodes => (SELECT id FROM gnodes), edges => (SELECT src, dst FROM gedges))",
        "island_id",
    );

    assert_eq!(map.len(), 6, "all nodes assigned: {map:?}");
    let islands: BTreeSet<i64> = map.values().copied().collect();
    assert_eq!(islands.len(), 2, "two islands: {islands:?}");
    assert_eq!(map["1"], map["2"]);
    assert_eq!(map["2"], map["3"]);
    assert_eq!(map["4"], map["5"]);
    assert_eq!(map["5"], map["6"]);
    assert_ne!(map["1"], map["4"], "the two components are distinct");
}

#[test]
fn louvain_inline_over_plain_tables_recovers_two_communities() {
    let db = PersistentDbPath::new("issue_799_louvain_inline");
    let rt = db.open_runtime();
    setup_tables(&rt);

    // Two triangles joined by a single bridge edge.
    for id in 1..=6 {
        insert_node(&rt, id);
    }
    // clique A {1,2,3}
    insert_edge(&rt, 1, 2);
    insert_edge(&rt, 1, 3);
    insert_edge(&rt, 2, 3);
    // clique B {4,5,6}
    insert_edge(&rt, 4, 5);
    insert_edge(&rt, 4, 6);
    insert_edge(&rt, 5, 6);
    // thin bridge
    insert_edge(&rt, 3, 4);

    let map = run_inline(
        &rt,
        "SELECT * FROM louvain(nodes => (SELECT id FROM gnodes), edges => (SELECT src, dst FROM gedges))",
        "community_id",
    );

    assert_eq!(map.len(), 6);
    let communities: BTreeSet<i64> = map.values().copied().collect();
    assert_eq!(communities.len(), 2, "two communities: {communities:?}");
    assert_eq!(map["1"], map["2"]);
    assert_eq!(map["2"], map["3"]);
    assert_eq!(map["4"], map["5"]);
    assert_eq!(map["5"], map["6"]);
    assert_ne!(map["1"], map["4"]);

    // The resolution named arg coexists with the inline subqueries.
    let map2 = run_inline(
        &rt,
        "SELECT * FROM louvain(nodes => (SELECT id FROM gnodes), edges => (SELECT src, dst FROM gedges), resolution => 1.0)",
        "community_id",
    );
    assert_eq!(map, map2, "resolution => 1.0 matches the default");
}

#[test]
fn degree_centrality_inline_over_plain_tables_counts_endpoints() {
    let db = PersistentDbPath::new("issue_799_degree_inline");
    let rt = db.open_runtime();
    setup_tables(&rt);

    // Star: 1 connected to 2 and 3.
    for id in 1..=3 {
        insert_node(&rt, id);
    }
    insert_edge(&rt, 1, 2);
    insert_edge(&rt, 1, 3);

    let map = run_inline(
        &rt,
        "SELECT * FROM degree_centrality(nodes => (SELECT id FROM gnodes), edges => (SELECT src, dst FROM gedges))",
        "degree",
    );

    assert_eq!(map["1"], 2, "hub has degree 2");
    assert_eq!(map["2"], 1);
    assert_eq!(map["3"], 1);
}

#[test]
fn inline_edges_wrong_shape_is_a_clear_error() {
    let db = PersistentDbPath::new("issue_799_wrong_shape");
    let rt = db.open_runtime();
    setup_tables(&rt);
    insert_node(&rt, 1);
    insert_node(&rt, 2);
    insert_edge(&rt, 1, 2);

    // The `edges` subquery projects a single column — not enough to form
    // (source, target).
    let err = rt
        .execute_query(
            "SELECT * FROM components(nodes => (SELECT id FROM gnodes), edges => (SELECT src FROM gedges))",
        )
        .expect_err("single-column edges must error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("two columns") || msg.contains("source, target"),
        "error should name the edges shape, got: {msg}"
    );
}

#[test]
fn inline_result_cache_is_scoped_to_source_collections() {
    let db = PersistentDbPath::new("issue_799_inline_cache");
    let rt = db.open_runtime();
    setup_tables(&rt);

    // Small graph (<= 5 result rows so it is eligible for the result cache):
    // two disjoint pairs {1,2} and {3,4}.
    for id in 1..=4 {
        insert_node(&rt, id);
    }
    insert_edge(&rt, 1, 2);
    insert_edge(&rt, 3, 4);

    let sql = "SELECT * FROM components(nodes => (SELECT id FROM gnodes), edges => (SELECT src, dst FROM gedges))";

    // First run: two components. Populates the result cache.
    let first = run_inline(&rt, sql, "island_id");
    let islands: BTreeSet<i64> = first.values().copied().collect();
    assert_eq!(islands.len(), 2, "two components before the merge");

    // An identical call returns identical content (consistent under caching).
    let repeat = run_inline(&rt, sql, "island_id");
    assert_eq!(first, repeat, "identical input yields identical output");

    // Mutating the `edges` source collection must invalidate the cache (its
    // cache key is scoped to the source collections), so the merge is visible.
    insert_edge(&rt, 2, 3);
    let merged = run_inline(&rt, sql, "island_id");
    let merged_islands: BTreeSet<i64> = merged.values().copied().collect();
    assert_eq!(
        merged_islands.len(),
        1,
        "after merging, all four nodes share one component: {merged:?}"
    );
}
