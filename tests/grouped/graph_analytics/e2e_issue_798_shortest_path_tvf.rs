//! End-to-end test for the `shortest_path(<graph>, src => <node_id>, dst =>
//! <node_id> [, max_hops => <i64>])` table-valued function (#798).
//!
//! Builds a small weighted graph via the real supported SQL `INSERT INTO <c>
//! NODE/EDGE` path, runs `SELECT * FROM shortest_path(...)` through the runtime
//! query path, and asserts the result projects ordered
//! `(hop, node_id, cumulative_weight)` rows along the known minimum-weight
//! route — not a single nested value. Also verifies that an unreachable pair
//! returns ZERO rows (not an error), and exercises the optional `max_hops`
//! scalar named argument end to end.
//!
//! Known v0 limitation (shared with `components` #795 / `louvain` #796): the
//! collection argument is NOT resolved to a named collection — the TVF runs
//! over the WHOLE graph store. This test therefore places the whole structure
//! in one graph collection.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use support::PersistentDbPath;

/// One projected path row: (hop, node_id, cumulative_weight).
#[derive(Debug, PartialEq)]
struct PathRow {
    hop: i64,
    node_id: String,
    cumulative_weight: f64,
}

/// Run the shortest_path TVF and return the projected path rows in order.
fn run_shortest_path(rt: &RedDBRuntime, sql: &str) -> Vec<PathRow> {
    let result = rt.execute_query(sql).expect("shortest_path query");
    assert_eq!(
        result.engine, "runtime-graph-tvf",
        "shortest_path(...) must route through the TVF executor"
    );
    let unified = result.result;

    // SELECT * must return the three columns in order — the path is returned as
    // ordered rows, not a single nested value.
    assert_eq!(
        unified.columns,
        vec![
            "hop".to_string(),
            "node_id".to_string(),
            "cumulative_weight".to_string()
        ],
        "columns must be hop, node_id, cumulative_weight"
    );

    unified
        .records
        .iter()
        .map(|rec| {
            let hop = match rec.get("hop").expect("hop present") {
                Value::Integer(n) => *n,
                other => panic!("hop should be Integer, got {other:?}"),
            };
            let node_id = match rec.get("node_id").expect("node_id present") {
                Value::Text(s) => s.to_string(),
                other => panic!("node_id should be Text, got {other:?}"),
            };
            let cumulative_weight = match rec.get("cumulative_weight").expect("weight present") {
                Value::Float(w) => *w,
                Value::Integer(w) => *w as f64,
                other => panic!("cumulative_weight should be Float, got {other:?}"),
            };
            PathRow {
                hop,
                node_id,
                cumulative_weight,
            }
        })
        .collect()
}

#[test]
fn shortest_path_tvf_returns_ordered_path_rows() {
    let db = PersistentDbPath::new("issue_798_shortest_path_tvf");
    let rt = db.open_runtime();

    // Diamond graph (undirected):
    //   a-b (1), a-c (4), b-c (1), c-d (1), b-d (5)
    // a -> d minimum-weight route is a-b-c-d, total weight 3.
    let a = make_node(&rt, "g", "a");
    let b = make_node(&rt, "g", "b");
    let c = make_node(&rt, "g", "c");
    let d = make_node(&rt, "g", "d");
    make_edge(&rt, "g", &a, &b, 1.0);
    make_edge(&rt, "g", &a, &c, 4.0);
    make_edge(&rt, "g", &b, &c, 1.0);
    make_edge(&rt, "g", &c, &d, 1.0);
    make_edge(&rt, "g", &b, &d, 5.0);

    let rows = run_shortest_path(
        &rt,
        &format!("SELECT * FROM shortest_path(g, src => {a}, dst => {d})"),
    );

    // Four rows, hops 0..3, in order along a-b-c-d.
    let route: Vec<String> = rows.iter().map(|r| r.node_id.clone()).collect();
    assert_eq!(route, vec![a.clone(), b.clone(), c.clone(), d.clone()]);
    let hops: Vec<i64> = rows.iter().map(|r| r.hop).collect();
    assert_eq!(
        hops,
        vec![0, 1, 2, 3],
        "hop indices are 0-based and ordered"
    );

    // Cumulative weight: 0 at source, +1 (a-b), +1 (b-c), +1 (c-d) = 3 total.
    assert_eq!(rows[0].cumulative_weight, 0.0);
    assert_eq!(rows[1].cumulative_weight, 1.0);
    assert_eq!(rows[2].cumulative_weight, 2.0);
    assert_eq!(rows[3].cumulative_weight, 3.0, "total path weight");

    // Determinism: a second run yields identical rows.
    let rows2 = run_shortest_path(
        &rt,
        &format!("SELECT * FROM shortest_path(g, src => {a}, dst => {d})"),
    );
    assert_eq!(
        rows, rows2,
        "shortest_path must be deterministic across runs"
    );
}

#[test]
fn shortest_path_tvf_unreachable_pair_returns_zero_rows() {
    let db = PersistentDbPath::new("issue_798_shortest_path_unreachable");
    let rt = db.open_runtime();

    // Two disconnected edges: {a-b} and {c-d}. a cannot reach d.
    let a = make_node(&rt, "g", "a");
    let b = make_node(&rt, "g", "b");
    let c = make_node(&rt, "g", "c");
    let d = make_node(&rt, "g", "d");
    make_edge(&rt, "g", &a, &b, 1.0);
    make_edge(&rt, "g", &c, &d, 1.0);

    // Unreachable pair must return ZERO rows, not an error.
    let rows = run_shortest_path(
        &rt,
        &format!("SELECT * FROM shortest_path(g, src => {a}, dst => {d})"),
    );
    assert!(rows.is_empty(), "unreachable pair yields empty result set");
}

#[test]
fn shortest_path_tvf_max_hops_caps_the_route() {
    let db = PersistentDbPath::new("issue_798_shortest_path_max_hops");
    let rt = db.open_runtime();

    // Cheap 3-hop route a-b-c-d (weight 3) vs. an expensive direct a-d (weight
    // 10). With max_hops => 1 only the direct edge fits the budget.
    let a = make_node(&rt, "g", "a");
    let b = make_node(&rt, "g", "b");
    let c = make_node(&rt, "g", "c");
    let d = make_node(&rt, "g", "d");
    make_edge(&rt, "g", &a, &b, 1.0);
    make_edge(&rt, "g", &b, &c, 1.0);
    make_edge(&rt, "g", &c, &d, 1.0);
    make_edge(&rt, "g", &a, &d, 10.0);

    // Unbounded: the cheap 3-hop route wins (4 rows, total weight 3).
    let unbounded = run_shortest_path(
        &rt,
        &format!("SELECT * FROM shortest_path(g, src => {a}, dst => {d})"),
    );
    assert_eq!(unbounded.len(), 4);
    assert_eq!(unbounded.last().unwrap().cumulative_weight, 3.0);

    // max_hops => 1: only the direct shortcut fits (2 rows, total weight 10).
    let capped = run_shortest_path(
        &rt,
        &format!("SELECT * FROM shortest_path(g, src => {a}, dst => {d}, max_hops => 1)"),
    );
    assert_eq!(capped.len(), 2, "source + destination only");
    assert_eq!(capped[0].node_id, a);
    assert_eq!(capped[1].node_id, d);
    assert_eq!(capped.last().unwrap().cumulative_weight, 10.0);
}

/// Insert a graph node via SQL and return its assigned graph node id.
fn make_node(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    rt.execute_query(&format!(
        "INSERT INTO {collection} NODE (label, node_type) VALUES ('{label}', 'Host')"
    ))
    .unwrap_or_else(|err| panic!("insert node {label}: {err:?}"));
    graph_node_id(rt, collection, label)
}

/// Insert a weighted directed edge between two graph nodes via SQL. The TVF
/// treats edges as undirected.
fn make_edge(rt: &RedDBRuntime, collection: &str, from: &str, to: &str, weight: f64) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} EDGE (label, from, to, weight) VALUES ('connects', {from}, {to}, {weight})"
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
