//! End-to-end tests for `CREATE GRAPH ... WITH ANALYTICS (...)` and the
//! resulting `<graph>.<output>` virtual views (#800).
//!
//! Exercises the full SQL → parser → catalog → resolver → algorithm path:
//!   - the acceptance-criterion query matrix: `CREATE GRAPH g WITH ANALYTICS
//!     (communities, components, centrality)` then `SELECT * FROM g.communities
//!     / g.components / g.centrality` (AC7);
//!   - every enabled output resolves to a selectable virtual view with the
//!     algorithm's native row shape (AC3);
//!   - the view recomputes on demand when the underlying graph changes (AC4);
//!   - `analytics_config` is persisted in the catalog and survives a restart /
//!     crash-recovery cycle (AC1);
//!   - per-output `using`/option selection drives the concrete algorithm;
//!   - analytics views are virtual and never appear in `SHOW COLLECTIONS` (AC6);
//!   - an undeclared output is a clear error.
//!
//! Known v0 limitation (shared with the underlying TVFs, #795-#797): the graph
//! argument is not scoped — the algorithms run over the whole graph store, so
//! each test uses a single graph collection.

mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::{BTreeMap, BTreeSet};
use support::PersistentDbPath;

const CREATE_FULL: &str = "CREATE GRAPH g WITH ANALYTICS (communities, components, centrality)";

/// Two fully-connected triangles with NO bridge between them: connected
/// components and Louvain communities both see two groups, and every centrality
/// measure is well defined.
fn build_two_clusters(rt: &RedDBRuntime) -> (Vec<String>, Vec<String>) {
    let a: Vec<String> = (1..=3)
        .map(|n| make_node(rt, "g", &format!("a{n}")))
        .collect();
    let b: Vec<String> = (1..=3)
        .map(|n| make_node(rt, "g", &format!("b{n}")))
        .collect();
    clique(rt, "g", &a);
    clique(rt, "g", &b);
    (a, b)
}

#[test]
fn acceptance_matrix_create_graph_with_analytics_and_select_each_view() {
    let db = PersistentDbPath::new("issue_800_acceptance_matrix");
    let rt = db.open_runtime();
    rt.execute_query(CREATE_FULL)
        .expect("create graph with analytics");
    build_two_clusters(&rt);

    // communities → (node_id, community_id)
    let communities = run_view(&rt, "SELECT * FROM g.communities");
    assert_eq!(communities.columns, vec!["node_id", "community_id"]);
    assert_eq!(communities.records.len(), 6, "all six nodes assigned");
    let community_ids: BTreeSet<i64> = communities
        .records
        .iter()
        .map(|r| integer(r, "community_id"))
        .collect();
    assert_eq!(
        community_ids.len(),
        2,
        "two disconnected cliques → two communities"
    );

    // components → (node_id, island_id)
    let components = run_view(&rt, "SELECT * FROM g.components");
    assert_eq!(components.columns, vec!["node_id", "island_id"]);
    assert_eq!(components.records.len(), 6);
    let islands: BTreeSet<i64> = components
        .records
        .iter()
        .map(|r| integer(r, "island_id"))
        .collect();
    assert_eq!(islands.len(), 2, "two disconnected cliques → two islands");

    // centrality → (node_id, score); default algorithm is pagerank (sums to 1)
    let centrality = run_view(&rt, "SELECT * FROM g.centrality");
    assert_eq!(centrality.columns, vec!["node_id", "score"]);
    assert_eq!(centrality.records.len(), 6);
    let sum: f64 = centrality.records.iter().map(|r| float(r, "score")).sum();
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "default pagerank sums to 1, got {sum}"
    );
}

#[test]
fn views_recompute_on_demand_when_graph_changes() {
    // AC4: no caching this slice — the view reflects the current graph data.
    let db = PersistentDbPath::new("issue_800_recompute");
    let rt = db.open_runtime();
    rt.execute_query(CREATE_FULL)
        .expect("create graph with analytics");
    let a: Vec<String> = (1..=3)
        .map(|n| make_node(&rt, "g", &format!("a{n}")))
        .collect();
    clique(&rt, "g", &a);

    let before = run_view(&rt, "SELECT * FROM g.components");
    assert_eq!(
        before.records.len(),
        3,
        "one clique → three nodes, one island"
    );
    let islands_before: BTreeSet<i64> = before
        .records
        .iter()
        .map(|r| integer(r, "island_id"))
        .collect();
    assert_eq!(islands_before.len(), 1);

    // Add a second, disconnected clique.
    let b: Vec<String> = (1..=2)
        .map(|n| make_node(&rt, "g", &format!("b{n}")))
        .collect();
    clique(&rt, "g", &b);

    let after = run_view(&rt, "SELECT * FROM g.components");
    assert_eq!(
        after.records.len(),
        5,
        "five nodes after growth — recomputed"
    );
    let islands_after: BTreeSet<i64> = after
        .records
        .iter()
        .map(|r| integer(r, "island_id"))
        .collect();
    assert_eq!(
        islands_after.len(),
        2,
        "now two islands — view recomputed on demand"
    );
}

#[test]
fn analytics_config_survives_restart() {
    // AC1: the declared analytics config is WAL-backed — it must survive a
    // process restart / crash-recovery cycle.
    let db = PersistentDbPath::new("issue_800_durability");
    {
        let rt = db.open_runtime();
        rt.execute_query(
            "CREATE GRAPH g WITH ANALYTICS (communities (using = louvain, resolution = 1.5), components, centrality (using = pagerank, max_iterations = 100))",
        )
        .expect("create graph with analytics");
        // Drop the runtime here — simulates a crash/restart boundary.
    }

    let rt = db.open_runtime();
    let contracts = rt.db().collection_contracts();
    let contract = contracts
        .iter()
        .find(|c| c.name == "g")
        .expect("graph 'g' contract survives restart");
    assert_eq!(
        contract.analytics_config.len(),
        3,
        "all three declared outputs survive recovery"
    );

    use reddb::catalog::AnalyticsOutput;
    let communities = contract
        .analytics_config
        .iter()
        .find(|v| v.output == AnalyticsOutput::Communities)
        .expect("communities survives");
    assert_eq!(communities.algorithm.as_deref(), Some("louvain"));
    assert_eq!(communities.resolution, Some(1.5));

    let centrality = contract
        .analytics_config
        .iter()
        .find(|v| v.output == AnalyticsOutput::Centrality)
        .expect("centrality survives");
    assert_eq!(centrality.algorithm.as_deref(), Some("pagerank"));
    assert_eq!(centrality.max_iterations, Some(100));

    // And the view is still resolvable against the recovered config.
    build_two_clusters(&rt);
    let components = run_view(&rt, "SELECT * FROM g.components");
    assert_eq!(components.columns, vec!["node_id", "island_id"]);
    assert_eq!(components.records.len(), 6);
}

#[test]
fn centrality_using_betweenness_selects_that_algorithm() {
    let db = PersistentDbPath::new("issue_800_using_betweenness");
    let rt = db.open_runtime();
    rt.execute_query("CREATE GRAPH g WITH ANALYTICS (centrality (using = betweenness))")
        .expect("create graph with betweenness centrality");
    // Hub-and-spoke: the hub must carry strictly positive betweenness.
    let hub = make_node(&rt, "g", "hub");
    let leaves: Vec<String> = (1..=3)
        .map(|n| make_node(&rt, "g", &format!("l{n}")))
        .collect();
    for leaf in &leaves {
        make_edge(&rt, "g", &hub, leaf);
    }

    let view = run_view(&rt, "SELECT * FROM g.centrality");
    assert_eq!(view.columns, vec!["node_id", "score"]);
    let scores: BTreeMap<String, f64> = view
        .records
        .iter()
        .map(|r| (text(r, "node_id"), float(r, "score")))
        .collect();
    assert!(
        scores[&hub] > 0.0,
        "hub has positive betweenness: {scores:?}"
    );
    for leaf in &leaves {
        assert!(scores[&hub] >= scores[leaf], "hub is the most central");
    }
}

#[test]
fn analytics_views_never_appear_in_show_collections() {
    // AC6 / directive: analytics outputs resolve as virtual `<graph>.<output>`
    // views — they are NOT registered as top-level collections, so they never
    // pollute `SHOW COLLECTIONS`, not even under `INCLUDING INTERNAL`. Only the
    // parent graph collection `g` is listed.
    let db = PersistentDbPath::new("issue_800_show_collections");
    let rt = db.open_runtime();
    rt.execute_query(CREATE_FULL)
        .expect("create graph with analytics");

    let view_names = ["g.communities", "g.components", "g.centrality"];

    for sql in ["SHOW COLLECTIONS", "SHOW COLLECTIONS INCLUDING INTERNAL"] {
        let result = rt
            .execute_query(sql)
            .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
        let names: BTreeSet<String> = result
            .result
            .records
            .iter()
            .filter_map(|record| match record.get("name") {
                Some(Value::Text(name)) => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert!(
            names.contains("g"),
            "{sql} lists the parent graph collection: {names:?}"
        );
        for view in view_names {
            assert!(
                !names.contains(view),
                "{sql} must not list the virtual analytics view '{view}': {names:?}"
            );
        }
    }
}

#[test]
fn undeclared_output_is_a_clear_error() {
    let db = PersistentDbPath::new("issue_800_undeclared_output");
    let rt = db.open_runtime();
    // Only `components` is enabled.
    rt.execute_query("CREATE GRAPH g WITH ANALYTICS (components)")
        .expect("create graph with one output");
    make_node(&rt, "g", "x");

    let err = rt
        .execute_query("SELECT * FROM g.communities")
        .expect_err("communities is not enabled");
    let message = format!("{err:?}");
    assert!(
        message.contains("not enabled"),
        "expected not-enabled error, got: {message}"
    );
}

/// Run a `<graph>.<output>` view query and assert it routes through the
/// analytics-view executor; return the unified result.
fn run_view(rt: &RedDBRuntime, sql: &str) -> reddb::storage::query::UnifiedResult {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(
        result.engine, "runtime-graph-analytics-view",
        "{sql} must route through the analytics-view executor"
    );
    result.result
}

fn integer(record: &reddb::storage::query::UnifiedRecord, column: &str) -> i64 {
    match record.get(column).expect("column present") {
        Value::Integer(n) => *n,
        other => panic!("{column} should be Integer, got {other:?}"),
    }
}

fn float(record: &reddb::storage::query::UnifiedRecord, column: &str) -> f64 {
    match record.get(column).expect("column present") {
        Value::Float(f) => *f,
        other => panic!("{column} should be Float, got {other:?}"),
    }
}

fn text(record: &reddb::storage::query::UnifiedRecord, column: &str) -> String {
    match record.get(column).expect("column present") {
        Value::Text(s) => s.to_string(),
        other => panic!("{column} should be Text, got {other:?}"),
    }
}

/// Insert a graph node via SQL and return its assigned graph node id.
fn make_node(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    rt.execute_query(&format!(
        "INSERT INTO {collection} NODE (label, node_type) VALUES ('{label}', 'Host')"
    ))
    .unwrap_or_else(|err| panic!("insert node {label}: {err:?}"));
    graph_node_id(rt, collection, label)
}

/// Insert an undirected-style edge between two graph nodes via SQL.
fn make_edge(rt: &RedDBRuntime, collection: &str, from: &str, to: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} EDGE (label, from, to, weight) VALUES ('connects', {from}, {to}, 1.0)"
    ))
    .unwrap_or_else(|err| panic!("insert edge {from}->{to}: {err:?}"));
}

/// Fully connect every pair of the given nodes.
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
