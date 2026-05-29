//! End-to-end test for issue #795: graph analytics `components(...)` TVF.
//!
//! This test drives the full stack: it creates two disconnected subgraphs in
//! the graph store via the real supported INSERT path (the same
//! `create_graph_node` / `create_graph_edge` helpers used by
//! `integration_graph_ops.rs`), runs `SELECT * FROM components(g)` through the
//! runtime query path, and asserts the result groups nodes into exactly two
//! islands with the correct membership.
//!
//! Known v0 limitation: the `components(<collection>)` argument is NOT resolved
//! to a named collection. The TVF runs over the WHOLE graph store regardless of
//! the argument value. Scoping the algorithm to a named collection is a
//! follow-up. This test therefore puts both subgraphs in the single in-memory
//! graph store and expects them to be discovered as two components.

mod support;

use serde_json::Value;
use std::collections::BTreeSet;
use support::{create_graph_edge, create_graph_node, TestContext};

/// Collect the distinct `island_id` values from a components result.
fn distinct_islands(result: &reddb_io::storage::query::UnifiedResult) -> BTreeSet<i64> {
    result
        .rows()
        .iter()
        .filter_map(|row| row.get("island_id").and_then(Value::as_i64))
        .collect()
}

/// Map node_id -> island_id for membership assertions.
fn membership(
    result: &reddb_io::storage::query::UnifiedResult,
) -> std::collections::BTreeMap<String, i64> {
    result
        .rows()
        .iter()
        .filter_map(|row| {
            let node = row.get("node_id").and_then(Value::as_str)?;
            let island = row.get("island_id").and_then(Value::as_i64)?;
            Some((node.to_string(), island))
        })
        .collect()
}

#[test]
fn components_tvf_groups_two_disconnected_subgraphs() {
    let ctx = TestContext::new();

    // Subgraph A: 1-2-3 mutually connected.
    create_graph_node(&ctx, "1", "n");
    create_graph_node(&ctx, "2", "n");
    create_graph_node(&ctx, "3", "n");
    create_graph_edge(&ctx, "1", "2");
    create_graph_edge(&ctx, "2", "3");

    // Subgraph B: 4-5 connected.
    create_graph_node(&ctx, "4", "n");
    create_graph_node(&ctx, "5", "n");
    create_graph_edge(&ctx, "4", "5");

    // Run the TVF.
    let result = ctx.query("SELECT * FROM components(g)");
    assert_eq!(result.engine, "runtime-graph-tvf");
    assert_eq!(result.result.columns, vec!["node_id", "island_id"]);
    assert_eq!(result.result.row_count(), 5);

    // Exactly two distinct island_ids.
    let islands = distinct_islands(&result.result);
    assert_eq!(islands.len(), 2, "expected two connected components");

    // Correct membership grouping: {1,2,3} share an island; {4,5} share the
    // other island; the two islands differ.
    let m = membership(&result.result);
    assert_eq!(m["1"], m["2"]);
    assert_eq!(m["2"], m["3"]);
    assert_eq!(m["4"], m["5"]);
    assert_ne!(m["1"], m["4"]);

    // Determinism: a second run yields identical rows.
    let result2 = ctx.query("SELECT * FROM components(g)");
    assert_eq!(result.result.rows(), result2.result.rows());
}
