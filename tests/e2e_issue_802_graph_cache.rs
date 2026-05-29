//! End-to-end tests for the graph-analytics result cache with event-driven
//! invalidation, the configurable TTL / capacity knobs, the kill-switch, and
//! the hit/miss/evict metrics (#802).
//!
//! These drive the graph-collection TVF form (`SELECT * FROM louvain(g)`) — the
//! form that, unlike the inline `nodes => / edges =>` form (#799), was not
//! cached before this slice. The acceptance criteria exercised here:
//!   - cache hit on a repeated identical call against unchanged source data;
//!   - cache miss after a mutation on the source graph collection;
//!   - eviction respects configured capacity and TTL;
//!   - hit/miss/evict counters with stable names move as expected;
//!   - the config kill-switch disables caching cleanly.
//!
//! Known v0 limitation (shared with #795/#796): `louvain(g)` runs over the
//! WHOLE graph store, so each test isolates its structure in one runtime.

mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::{BTreeMap, BTreeSet};
use support::PersistentDbPath;

/// Run `louvain(g)` and return a map of node_id -> community_id.
fn run_louvain(rt: &RedDBRuntime, sql: &str) -> BTreeMap<String, i64> {
    let result = rt.execute_query(sql).expect("louvain query");
    assert_eq!(
        result.engine, "runtime-graph-tvf",
        "louvain(...) must route through the graph-collection TVF executor"
    );
    let unified = result.result;
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

/// Build the canonical two-community graph in collection `g`: clusters
/// {1..4} and {5..8} joined by a single thin bridge. Returns the node ids.
fn seed_two_communities(rt: &RedDBRuntime) -> Vec<String> {
    let ids: Vec<String> = (1..=8)
        .map(|n| make_node(rt, "g", &n.to_string()))
        .collect();
    clique(rt, "g", &ids[0..4]);
    clique(rt, "g", &ids[4..8]);
    make_edge(rt, "g", &ids[3], &ids[4]); // thin bridge
    ids
}

#[test]
fn louvain_cache_hits_then_misses_after_mutation() {
    let db = PersistentDbPath::new("issue_802_louvain_cache");
    let rt = db.open_runtime();
    let ids = seed_two_communities(&rt);
    let sql = "SELECT * FROM louvain(g)";

    // First run is a cold compute → one miss, no hit. (More than 5 rows are
    // returned, proving graph-TVF output is cached past the generic ≤5-row
    // heuristic.)
    let (h0, m0, _) = rt.result_cache_metrics();
    let first = run_louvain(&rt, sql);
    assert_eq!(first.len(), 8, "all eight nodes assigned");
    let (h1, m1, _) = rt.result_cache_metrics();
    assert_eq!(h1, h0, "cold compute is not a hit");
    assert_eq!(m1, m0 + 1, "cold compute records exactly one miss");

    // An identical call against unchanged source data is a cache HIT — no
    // intervening query runs between the two calls.
    let repeat = run_louvain(&rt, sql);
    let (h2, m2, _) = rt.result_cache_metrics();
    assert_eq!(h2, h1 + 1, "identical repeated call is a cache hit");
    assert_eq!(m2, m1, "the hit records no additional miss");
    assert_eq!(
        first, repeat,
        "cached result is identical to the cold result"
    );

    // Mutate the source graph collection `g` (add a ninth node + edge). The
    // INSERTs invalidate every cache entry scoped to `g`, so the next call
    // must recompute. The result now covers nine nodes — a stale cache would
    // still report eight.
    let n9 = make_node(&rt, "g", "9");
    make_edge(&rt, "g", &ids[7], &n9);
    let (h_after_mut, _, _) = rt.result_cache_metrics();

    let after = run_louvain(&rt, sql);
    let (h3, _, _) = rt.result_cache_metrics();
    assert_eq!(
        h3, h_after_mut,
        "post-mutation call is a miss (hit counter does not advance)"
    );
    assert_eq!(after.len(), 9, "recomputed result includes the new node");
}

#[test]
fn result_cache_kill_switch_disables_cache_cleanly() {
    let db = PersistentDbPath::new("issue_802_kill_switch");
    let rt = db.open_runtime();
    seed_two_communities(&rt);

    // Flip the kill-switch off.
    rt.db()
        .store()
        .set_config_tree("runtime.result_cache.enabled", &reddb::json!(false));

    let sql = "SELECT * FROM louvain(g)";
    let (h0, m0, e0) = rt.result_cache_metrics();
    let first = run_louvain(&rt, sql);
    let second = run_louvain(&rt, sql);
    let (h1, m1, e1) = rt.result_cache_metrics();

    // With caching disabled, neither read is counted as a hit or a miss —
    // the cache is bypassed entirely — yet results stay correct.
    assert_eq!(
        (h1, m1, e1),
        (h0, m0, e0),
        "disabled cache is fully bypassed"
    );
    assert_eq!(
        first, second,
        "results remain deterministic without caching"
    );
    assert_eq!(first.len(), 8);
}

#[test]
fn result_cache_ttl_zero_forces_recompute() {
    let db = PersistentDbPath::new("issue_802_ttl_zero");
    let rt = db.open_runtime();
    seed_two_communities(&rt);

    // A zero-second TTL means every entry is already expired on read.
    rt.db()
        .store()
        .set_config_tree("runtime.result_cache.ttl_seconds", &reddb::json!(0));

    let sql = "SELECT * FROM louvain(g)";
    let (h0, m0, _) = rt.result_cache_metrics();
    let first = run_louvain(&rt, sql);
    let second = run_louvain(&rt, sql);
    let (h1, m1, _) = rt.result_cache_metrics();

    // Both runs miss (the entry expires immediately), so the TTL knob is
    // respected; results are still correct and identical.
    assert_eq!(h1, h0, "zero TTL never yields a hit");
    assert_eq!(m1, m0 + 2, "both runs are misses under a zero TTL");
    assert_eq!(first, second);
    let communities: BTreeSet<i64> = first.values().copied().collect();
    assert_eq!(communities.len(), 2);
}

#[test]
fn result_cache_capacity_evicts_and_counts() {
    let db = PersistentDbPath::new("issue_802_capacity");
    let rt = db.open_runtime();
    seed_two_communities(&rt);

    // Capacity of one entry: caching a second distinct graph-TVF query evicts
    // the first.
    rt.db()
        .store()
        .set_config_tree("runtime.result_cache.capacity_entries", &reddb::json!(1));

    let (_, _, e0) = rt.result_cache_metrics();
    // Two distinct cacheable graph-TVF queries over the same graph.
    let _ = rt
        .execute_query("SELECT * FROM louvain(g)")
        .expect("louvain query");
    let _ = rt
        .execute_query("SELECT * FROM components(g)")
        .expect("components query");
    let (_, _, e1) = rt.result_cache_metrics();

    assert!(
        e1 >= e0 + 1,
        "caching a second entry beyond capacity 1 evicts the first ({e0} -> {e1})"
    );
}

// ── Graph-backed TVF helpers (mirrors #796) ──────────────────────────────

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
