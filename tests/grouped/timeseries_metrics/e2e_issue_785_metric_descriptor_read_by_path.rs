//! Issue #785 — Expose metric descriptor reads by exact path and prefix.
//!
//! The metric descriptor catalog already persists state via `CREATE METRIC`
//! (issue #784) and projects it back through the `red.analytics.metrics`
//! virtual table. This suite pins the read contract:
//!
//! * a single descriptor is reachable by exact dotted path through a normal
//!   `WHERE path = ...` filter;
//! * a namespace of descriptors is reachable by stable dotted path prefix
//!   through `WHERE path STARTS WITH ...` and the equivalent SQL `LIKE`
//!   pattern;
//! * the prefix surface only exposes the descriptor's stable taxonomy
//!   columns (`path`, `kind`, `role`, `created_at`) and does not leak any
//!   high-cardinality dimension columns;
//! * a no-match prefix yields an empty result rather than an error.
//!
//! All reads go through `SELECT FROM red.analytics.metrics` — i.e. normal
//! query/catalog semantics, not a separate `SHOW`-style command.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn seed_descriptors(rt: &RedDBRuntime) {
    for sql in [
        "CREATE METRIC infra.database.cpu.usage TYPE gauge ROLE operational",
        "CREATE METRIC infra.database.cpu.load TYPE gauge ROLE operational",
        "CREATE METRIC infra.api.request_count TYPE counter ROLE operational",
        "CREATE METRIC product.daily_active_users TYPE gauge ROLE kpi",
    ] {
        exec(rt, sql);
    }
}

fn paths(result: &reddb::runtime::RuntimeQueryResult) -> Vec<String> {
    let mut paths: Vec<String> = result
        .result
        .records
        .iter()
        .map(|row| text(row, "path").to_string())
        .collect();
    paths.sort();
    paths
}

#[test]
fn exact_path_read_returns_only_matching_descriptor() {
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT path, kind, role FROM red.analytics.metrics \
         WHERE path = 'infra.database.cpu.usage'",
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(text(row, "path"), "infra.database.cpu.usage");
    assert_eq!(text(row, "kind"), "gauge");
    assert_eq!(text(row, "role"), "operational");
}

#[test]
fn exact_path_read_for_unknown_path_returns_empty() {
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path = 'infra.database.does_not_exist'",
    );
    assert!(result.result.records.is_empty());
}

#[test]
fn prefix_read_with_starts_with_lists_namespace_descriptors() {
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path STARTS WITH 'infra.database'",
    );
    assert_eq!(
        paths(&result),
        vec![
            "infra.database.cpu.load".to_string(),
            "infra.database.cpu.usage".to_string(),
        ]
    );
}

#[test]
fn prefix_read_with_like_pattern_matches_starts_with() {
    let rt = runtime();
    seed_descriptors(&rt);

    let starts_with = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path STARTS WITH 'infra'",
    );
    let like = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path LIKE 'infra.%'",
    );
    assert_eq!(paths(&starts_with), paths(&like));
    assert_eq!(
        paths(&starts_with),
        vec![
            "infra.api.request_count".to_string(),
            "infra.database.cpu.load".to_string(),
            "infra.database.cpu.usage".to_string(),
        ]
    );
}

#[test]
fn prefix_read_only_exposes_stable_taxonomy_columns() {
    // Prefix reads must preserve the path/dimension boundary documented in
    // the analytics ontology: the catalog row surfaces stable taxonomy only
    // (path, kind, role, created_at). High-cardinality dimensions are never
    // part of the descriptor row and must not appear in the projection.
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT * FROM red.analytics.metrics \
         WHERE path STARTS WITH 'infra'",
    );
    let columns: std::collections::HashSet<&str> =
        result.result.columns.iter().map(String::as_str).collect();
    assert_eq!(
        columns,
        [
            "path",
            "kind",
            "role",
            "created_at",
            // Issue #790 — derived metric descriptor metadata. NULL on
            // raw (non-derived) descriptors but always part of the
            // taxonomy projection.
            "source",
            "query",
            "window_ms",
            "time_field",
        ]
        .into_iter()
        .collect::<std::collections::HashSet<_>>(),
        "prefix reads must only expose stable descriptor columns, got {:?}",
        result.result.columns
    );
    for forbidden in ["dimensions", "labels", "host", "instance", "value"] {
        assert!(
            !columns.contains(forbidden),
            "prefix read leaked high-cardinality column `{forbidden}`"
        );
    }
}

#[test]
fn prefix_read_with_no_match_returns_empty_not_error() {
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path STARTS WITH 'security.audit'",
    );
    assert!(result.result.records.is_empty());
}

#[test]
fn empty_string_prefix_matches_every_descriptor() {
    // An empty-prefix read should be a valid query and return the full
    // catalog rather than failing, since every dotted path starts with the
    // empty string. This pins the boundary so empty inputs don't surprise
    // callers with errors.
    let rt = runtime();
    seed_descriptors(&rt);

    let result = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics WHERE path STARTS WITH ''",
    );
    assert_eq!(result.result.records.len(), 4);
}
