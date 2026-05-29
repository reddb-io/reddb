//! End-to-end tests for `ALTER GRAPH ... ADD|DROP ANALYTICS` (#801).
//!
//! Lifecycle management of the `WITH ANALYTICS` configuration declared at
//! `CREATE GRAPH` time (#800), without recreating the collection. Exercises
//! the full SQL → parser → catalog → resolver path:
//!   - `ADD ANALYTICS (...)` parses and mutates `analytics_config` durably (AC1);
//!   - `DROP ANALYTICS <output>` parses and mutates `analytics_config` durably (AC2);
//!   - adding an already-enabled output is a no-op (AC3);
//!   - dropping an output that is not enabled is a clear error (AC4);
//!   - the integration matrix: create graph without analytics, add `communities`,
//!     read `g.communities`, drop `communities`, confirm the view no longer
//!     resolves (AC6).
//!
//! Known v0 limitation (shared with #800 / #795-#797): the graph argument is
//! not scoped — the algorithms run over the whole graph store, so each test
//! uses a single graph collection.

mod support;

use reddb::catalog::AnalyticsOutput;
use reddb::RedDBRuntime;
use support::PersistentDbPath;

#[test]
fn add_then_drop_analytics_round_trips_the_view() {
    // AC6 — the full lifecycle matrix on one graph.
    let db = PersistentDbPath::new("issue_801_add_drop_matrix");
    let rt = db.open_runtime();

    // Create the graph with NO analytics config.
    rt.execute_query("CREATE GRAPH g")
        .expect("create graph without analytics");
    build_two_clusters(&rt);

    // The view does not resolve before the output is enabled.
    let err = rt
        .execute_query("SELECT * FROM g.communities")
        .expect_err("communities not enabled yet");
    assert!(
        format!("{err:?}").contains("not enabled"),
        "expected not-enabled error before ADD, got: {err:?}"
    );

    // ADD ANALYTICS enables the output; the next read materializes it.
    rt.execute_query("ALTER GRAPH g ADD ANALYTICS (communities)")
        .expect("add communities analytics");
    assert_eq!(enabled_outputs(&rt), vec![AnalyticsOutput::Communities]);

    let view = run_view(&rt, "SELECT * FROM g.communities");
    assert_eq!(view.columns, vec!["node_id", "community_id"]);
    assert_eq!(view.records.len(), 6, "all six nodes assigned a community");

    // DROP ANALYTICS removes the output; the view stops resolving.
    rt.execute_query("ALTER GRAPH g DROP ANALYTICS communities")
        .expect("drop communities analytics");
    assert!(enabled_outputs(&rt).is_empty(), "config emptied after drop");

    let err = rt
        .execute_query("SELECT * FROM g.communities")
        .expect_err("communities no longer enabled");
    assert!(
        format!("{err:?}").contains("not enabled"),
        "expected not-enabled error after DROP, got: {err:?}"
    );
}

#[test]
fn add_analytics_is_idempotent() {
    // AC3 — adding an already-enabled output is a no-op: no error and no
    // duplicate state. The first declaration's options are preserved.
    let db = PersistentDbPath::new("issue_801_add_idempotent");
    let rt = db.open_runtime();
    rt.execute_query("CREATE GRAPH g").expect("create graph");
    rt.execute_query(
        "ALTER GRAPH g ADD ANALYTICS (communities (using = louvain, resolution = 1.5))",
    )
    .expect("add communities");

    // Re-adding the same output must not error and must not duplicate or
    // overwrite the existing descriptor.
    rt.execute_query("ALTER GRAPH g ADD ANALYTICS (communities (using = label_propagation))")
        .expect("re-add communities is a no-op");

    let config = analytics_config(&rt);
    assert_eq!(config.len(), 1, "no duplicate output entry");
    assert_eq!(config[0].output, AnalyticsOutput::Communities);
    assert_eq!(
        config[0].algorithm.as_deref(),
        Some("louvain"),
        "first declaration wins; re-add does not overwrite options"
    );
    assert_eq!(config[0].resolution, Some(1.5));
}

#[test]
fn drop_absent_analytics_is_a_clear_error() {
    // AC4 — dropping an output that was never enabled is an explicit error,
    // not a silent no-op.
    let db = PersistentDbPath::new("issue_801_drop_absent");
    let rt = db.open_runtime();
    rt.execute_query("CREATE GRAPH g WITH ANALYTICS (components)")
        .expect("create graph with components only");

    let err = rt
        .execute_query("ALTER GRAPH g DROP ANALYTICS communities")
        .expect_err("communities was never enabled");
    let message = format!("{err:?}");
    assert!(
        message.contains("not enabled"),
        "expected not-enabled error, got: {message}"
    );

    // The unrelated, enabled output is untouched.
    assert_eq!(enabled_outputs(&rt), vec![AnalyticsOutput::Components]);
}

#[test]
fn analytics_mutations_survive_restart() {
    // AC1 / AC2 — both ADD and DROP mutate the WAL-backed config durably.
    let db = PersistentDbPath::new("issue_801_durability");
    {
        let rt = db.open_runtime();
        rt.execute_query("CREATE GRAPH g WITH ANALYTICS (components)")
            .expect("create graph");
        rt.execute_query(
            "ALTER GRAPH g ADD ANALYTICS (communities (using = louvain), centrality (using = pagerank, max_iterations = 50))",
        )
        .expect("add two outputs");
        rt.execute_query("ALTER GRAPH g DROP ANALYTICS components")
            .expect("drop the original output");
        // Drop the runtime — simulates a crash / restart boundary.
    }

    let rt = db.open_runtime();
    let config = analytics_config(&rt);
    assert_eq!(
        enabled_outputs(&rt),
        vec![AnalyticsOutput::Communities, AnalyticsOutput::Centrality],
        "ADD survives and DROP survives across restart"
    );
    let centrality = config
        .iter()
        .find(|v| v.output == AnalyticsOutput::Centrality)
        .expect("centrality survives");
    assert_eq!(centrality.algorithm.as_deref(), Some("pagerank"));
    assert_eq!(centrality.max_iterations, Some(50));
}

#[test]
fn alter_graph_analytics_rejects_non_graph_collection() {
    // Analytics lifecycle is graph-only — the op fails loudly on a table.
    let db = PersistentDbPath::new("issue_801_non_graph");
    let rt = db.open_runtime();
    rt.execute_query("CREATE TABLE t (id INT)")
        .expect("create table");
    let err = rt
        .execute_query("ALTER GRAPH t ADD ANALYTICS (communities)")
        .expect_err("t is not a graph");
    assert!(
        format!("{err:?}").contains("not a graph"),
        "expected not-a-graph error, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The persisted analytics config on graph `g`, in declared order.
fn analytics_config(rt: &RedDBRuntime) -> Vec<reddb::catalog::AnalyticsViewDescriptor> {
    rt.db()
        .collection_contracts()
        .iter()
        .find(|c| c.name == "g")
        .expect("graph 'g' contract present")
        .analytics_config
        .clone()
}

/// The enabled analytics outputs on graph `g`, in declared order.
fn enabled_outputs(rt: &RedDBRuntime) -> Vec<AnalyticsOutput> {
    analytics_config(rt).iter().map(|v| v.output).collect()
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

/// Two fully-connected triangles with no bridge — two communities, two
/// components, well-defined centrality (mirrors the #800 fixture).
fn build_two_clusters(rt: &RedDBRuntime) {
    let a: Vec<String> = (1..=3)
        .map(|n| make_node(rt, "g", &format!("a{n}")))
        .collect();
    let b: Vec<String> = (1..=3)
        .map(|n| make_node(rt, "g", &format!("b{n}")))
        .collect();
    clique(rt, "g", &a);
    clique(rt, "g", &b);
}

fn make_node(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    rt.execute_query(&format!(
        "INSERT INTO {collection} NODE (label, node_type) VALUES ('{label}', 'Host')"
    ))
    .unwrap_or_else(|err| panic!("insert node {label}: {err:?}"));
    graph_node_id(rt, collection, label)
}

fn make_edge(rt: &RedDBRuntime, collection: &str, from: &str, to: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} EDGE (label, from, to, weight) VALUES ('connects', {from}, {to}, 1.0)"
    ))
    .unwrap_or_else(|err| panic!("insert edge {from}->{to}: {err:?}"));
}

fn clique(rt: &RedDBRuntime, collection: &str, nodes: &[String]) {
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            make_edge(rt, collection, &nodes[i], &nodes[j]);
        }
    }
}

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
        .unwrap_or_else(|| panic!("node '{label}' should exist in '{collection}'"))
}
