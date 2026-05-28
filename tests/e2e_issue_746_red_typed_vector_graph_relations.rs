//! Issue #746 — typed `red.*` relations for vectors and graphs.
//!
//! Pins the contract that the Red UI toolbars depend on:
//!
//! 1. `red.vectors` exposes vector-shaped columns
//!    (`name, dimensions, metric, vector_count, search_capable,
//!    artifact_state, in_memory_bytes, on_disk_bytes, tenant_id,
//!    internal`) — no table/document/graph noise.
//! 2. `red.graphs` exposes graph-shaped columns
//!    (`name, node_count, edge_count, node_labels, edge_labels,
//!    supports_viewport, supports_algorithms, in_memory_bytes,
//!    on_disk_bytes, internal`).
//!    `supports_viewport` / `supports_algorithms` are stable
//!    capability indicators (true — viewport contract landed in
//!    #744; graph algorithm commands are always available).
//! 3. Each relation only returns rows for its model — `red.vectors`
//!    skips graphs, `red.graphs` skips vectors, etc.
//! 4. Tenant scope is respected like other `red.*` surfaces: rows
//!    created under a tenant are hidden from a different tenant's
//!    session but visible to cluster admin.
//! 5. When the vector introspection registry (#743) has not been
//!    published yet, `red.vectors` surfaces explicit stable values
//!    (`artifact_state = 'unavailable'`, `search_capable = false`)
//!    rather than NULL — per the #746 thread-discussion decision.

use std::collections::HashSet;

use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBOptions;

const VECTOR_COLUMNS: [&str; 10] = [
    "name",
    "dimensions",
    "metric",
    "vector_count",
    "search_capable",
    "artifact_state",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
];

const GRAPH_COLUMNS: [&str; 10] = [
    "name",
    "node_count",
    "edge_count",
    "node_labels",
    "edge_labels",
    "supports_viewport",
    "supports_algorithms",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn select(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn find_row<'a>(records: &'a [UnifiedRecord], name: &str) -> Option<&'a UnifiedRecord> {
    records.iter().find(|record| match record.get("name") {
        Some(Value::Text(value)) => value.as_ref() == name,
        _ => false,
    })
}

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column `{column}`, got {other:?}"),
    }
}

fn uint(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned column `{column}`, got {other:?}"),
    }
}

fn boolean(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column `{column}`, got {other:?}"),
    }
}

fn names(records: &[UnifiedRecord]) -> HashSet<String> {
    records
        .iter()
        .filter_map(|record| match record.get("name") {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        })
        .collect()
}

fn assert_columns(actual: &[String], expected: &[&str]) {
    let actual: HashSet<&str> = actual.iter().map(String::as_str).collect();
    let expected: HashSet<&str> = expected.iter().copied().collect();
    assert_eq!(
        actual, expected,
        "column set must match the typed-relation contract"
    );
}

#[test]
fn red_vectors_exposes_vector_shaped_columns() {
    let rt = open_runtime();
    exec(&rt, "CREATE VECTOR embeddings DIM 3 METRIC cosine");

    let result = select(&rt, "SELECT * FROM red.vectors");
    assert_columns(&result.result.columns, &VECTOR_COLUMNS);

    let row = find_row(&result.result.records, "embeddings").expect("embeddings row");
    assert_eq!(uint(row, "dimensions"), 3);
    assert_eq!(text(row, "metric"), "cosine");
    // No artifact has been published yet — the typed relation must
    // surface explicit stable values per the #746 thread-discussion.
    assert_eq!(text(row, "artifact_state"), "unavailable");
    assert!(!boolean(row, "search_capable"));
    assert!(!boolean(row, "internal"));
}

#[test]
fn red_vectors_skips_tables_and_graphs() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE plain_table (id INT)");
    exec(&rt, "CREATE VECTOR plain_vec DIM 2 METRIC l2");
    exec(&rt, "CREATE GRAPH plain_graph");

    let result = select(&rt, "SELECT name FROM red.vectors");
    let names = names(&result.result.records);
    assert!(
        names.contains("plain_vec"),
        "plain_vec missing from red.vectors: {names:?}"
    );
    assert!(
        !names.contains("plain_table"),
        "red.vectors must not include tables: {names:?}"
    );
    assert!(
        !names.contains("plain_graph"),
        "red.vectors must not include graphs: {names:?}"
    );
}

#[test]
fn red_graphs_exposes_graph_shaped_columns() {
    let rt = open_runtime();
    exec(&rt, "CREATE GRAPH tales");
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );
    exec(
        &rt,
        "INSERT INTO tales EDGE (label, from, to) VALUES ('HAS_TRAIT', 'hansel', 'gretel')",
    );

    let result = select(&rt, "SELECT * FROM red.graphs");
    assert_columns(&result.result.columns, &GRAPH_COLUMNS);

    let row = find_row(&result.result.records, "tales").expect("tales row");
    assert_eq!(uint(row, "node_count"), 2);
    assert_eq!(uint(row, "edge_count"), 1);
    assert!(
        boolean(row, "supports_viewport"),
        "graph collections always support the viewport contract"
    );
    assert!(
        boolean(row, "supports_algorithms"),
        "graph algorithm commands are always available"
    );
    assert!(!boolean(row, "internal"));
}

#[test]
fn red_graphs_skips_tables_and_vectors() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE only_table (id INT)");
    exec(&rt, "CREATE VECTOR only_vec DIM 2 METRIC l2");
    exec(&rt, "CREATE GRAPH only_graph");

    let result = select(&rt, "SELECT name FROM red.graphs");
    let names = names(&result.result.records);
    assert!(
        names.contains("only_graph"),
        "only_graph missing: {names:?}"
    );
    assert!(
        !names.contains("only_table"),
        "red.graphs must not include tables: {names:?}"
    );
    assert!(
        !names.contains("only_vec"),
        "red.graphs must not include vector collections: {names:?}"
    );
}

#[test]
fn red_vector_and_graph_relations_respect_tenant_scope() {
    let rt = open_runtime();
    exec(&rt, "SET TENANT 'acme'");
    exec(&rt, "CREATE VECTOR acme_vecs DIM 2 METRIC cosine");
    exec(&rt, "CREATE GRAPH acme_graph");

    exec(&rt, "SET TENANT 'globex'");
    exec(&rt, "CREATE VECTOR globex_vecs DIM 4 METRIC l2");
    exec(&rt, "CREATE GRAPH globex_graph");

    // Globex session — only globex-owned rows visible.
    let vecs = names(&select(&rt, "SELECT name FROM red.vectors").result.records);
    assert!(
        vecs.contains("globex_vecs"),
        "globex sees its own vectors: {vecs:?}"
    );
    assert!(
        !vecs.contains("acme_vecs"),
        "globex must not see acme's vectors: {vecs:?}"
    );

    let graphs = names(&select(&rt, "SELECT name FROM red.graphs").result.records);
    assert!(
        graphs.contains("globex_graph"),
        "globex sees its own graph: {graphs:?}"
    );
    assert!(
        !graphs.contains("acme_graph"),
        "globex must not see acme's graph: {graphs:?}"
    );

    // Cluster admin (no tenant) sees everything.
    exec(&rt, "SET TENANT NULL");
    let admin_vecs = names(&select(&rt, "SELECT name FROM red.vectors").result.records);
    assert!(admin_vecs.contains("acme_vecs"));
    assert!(admin_vecs.contains("globex_vecs"));
    let admin_graphs = names(&select(&rt, "SELECT name FROM red.graphs").result.records);
    assert!(admin_graphs.contains("acme_graph"));
    assert!(admin_graphs.contains("globex_graph"));
}
