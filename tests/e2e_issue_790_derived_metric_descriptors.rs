//! Issue #790 — Store derived metric descriptors over collections without
//! executing them.
//!
//! A *derived* metric descriptor names the inputs a future execution layer
//! would consume: an analytics source profile, a free-form query
//! expression, an evaluation window, and an optional time-field override.
//! v0 persists the metadata only — there is no engine that turns these
//! into a value yet. This suite pins:
//!
//! * KPI-style derived descriptors (source + query + window) persist and
//!   survive a checkpoint/reopen cycle;
//! * SLI-style derived descriptors (with TIME_FIELD override) persist the
//!   same way;
//! * `READ METRIC <path>` (attempting to read the *output*) returns a
//!   structured "not yet implemented" response that names both the path
//!   and the v0 boundary, so callers can tell it apart from a missing
//!   descriptor;
//! * The descriptor itself is reachable through `red.analytics.metrics`
//!   even when the output read is unsupported.

mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use support::{checkpoint_and_reopen, PersistentDbPath};

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

fn integer(row: &reddb::storage::query::UnifiedRecord, field: &str) -> i64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected {field} integer, got {other:?} in {row:?}"),
    }
}

#[test]
fn kpi_derived_descriptor_persists_source_query_and_window() {
    let path = PersistentDbPath::new("derived_metric_kpi");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRIC product.daily_active_users \
         TYPE gauge ROLE kpi \
         SOURCE product_events \
         QUERY 'count_distinct(user_id)' \
         WINDOW 1 DAY",
    );

    let result = exec(
        &rt,
        "SELECT path, kind, role, source, query, window_ms, time_field \
         FROM red.analytics.metrics \
         WHERE path = 'product.daily_active_users'",
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(text(row, "path"), "product.daily_active_users");
    assert_eq!(text(row, "kind"), "gauge");
    assert_eq!(text(row, "role"), "kpi");
    assert_eq!(text(row, "source"), "product_events");
    assert_eq!(text(row, "query"), "count_distinct(user_id)");
    assert_eq!(integer(row, "window_ms"), 86_400_000);
    assert!(
        matches!(row.get("time_field"), Some(Value::Null) | None),
        "time_field should be NULL when not supplied, got {:?}",
        row.get("time_field")
    );

    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT source, query, window_ms FROM red.analytics.metrics \
         WHERE path = 'product.daily_active_users'",
    );
    assert_eq!(persisted.result.records.len(), 1);
    let row = &persisted.result.records[0];
    assert_eq!(text(row, "source"), "product_events");
    assert_eq!(text(row, "query"), "count_distinct(user_id)");
    assert_eq!(integer(row, "window_ms"), 86_400_000);
}

#[test]
fn sli_derived_descriptor_persists_time_field_override() {
    let path = PersistentDbPath::new("derived_metric_sli");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRIC infra.api.error_rate \
         TYPE ratio ROLE sli \
         SOURCE http_requests \
         QUERY 'sum(status >= 500) / count(*)' \
         WINDOW 5 MINUTES \
         TIME_FIELD received_at",
    );

    let result = exec(
        &rt,
        "SELECT role, source, query, window_ms, time_field \
         FROM red.analytics.metrics \
         WHERE path = 'infra.api.error_rate'",
    );
    let row = &result.result.records[0];
    assert_eq!(text(row, "role"), "sli");
    assert_eq!(text(row, "source"), "http_requests");
    assert_eq!(text(row, "query"), "sum(status >= 500) / count(*)");
    assert_eq!(integer(row, "window_ms"), 300_000);
    assert_eq!(text(row, "time_field"), "received_at");

    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT time_field, window_ms FROM red.analytics.metrics \
         WHERE path = 'infra.api.error_rate'",
    );
    let row = &persisted.result.records[0];
    assert_eq!(text(row, "time_field"), "received_at");
    assert_eq!(integer(row, "window_ms"), 300_000);
}

#[test]
fn raw_descriptor_leaves_derived_columns_null() {
    // Raw (non-derived) descriptors must still surface NULL for the new
    // derived-only columns; the catalog shape is uniform across roles.
    let rt = runtime();
    exec(
        &rt,
        "CREATE METRIC infra.database.cpu.usage TYPE gauge ROLE operational",
    );
    let result = exec(
        &rt,
        "SELECT source, query, window_ms, time_field \
         FROM red.analytics.metrics \
         WHERE path = 'infra.database.cpu.usage'",
    );
    let row = &result.result.records[0];
    for column in ["source", "query", "window_ms", "time_field"] {
        assert!(
            matches!(row.get(column), Some(Value::Null) | None),
            "expected NULL for {column} on a raw descriptor, got {:?}",
            row.get(column)
        );
    }
}

#[test]
fn read_metric_output_returns_not_yet_implemented() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE METRIC product.daily_active_users \
         TYPE gauge ROLE kpi \
         SOURCE product_events \
         QUERY 'count_distinct(user_id)' \
         WINDOW 1 DAY",
    );

    // Descriptor itself is reachable — the error path is for the value
    // read, not the descriptor read.
    let descriptor = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics \
         WHERE path = 'product.daily_active_users'",
    );
    assert_eq!(descriptor.result.records.len(), 1);

    let err = rt
        .execute_query("READ METRIC product.daily_active_users")
        .expect_err("READ METRIC must fail in Analytics v0")
        .to_string();
    assert!(
        err.contains("not yet implemented"),
        "expected 'not yet implemented' wording, got {err}"
    );
    assert!(
        err.contains("product.daily_active_users"),
        "expected the requested path to surface in the error, got {err}"
    );
    assert!(
        err.contains("Analytics v0"),
        "expected the v0 boundary to be named in the error, got {err}"
    );
}

#[test]
fn read_metric_output_unsupported_even_for_unknown_path() {
    // The v0 boundary fires before existence is consulted; we do not want
    // callers to use the error wording as a "does this exist?" oracle for
    // an unimplemented surface. Any READ METRIC reaches the same wall.
    let rt = runtime();
    let err = rt
        .execute_query("READ METRIC infra.never.declared")
        .expect_err("READ METRIC must fail regardless of descriptor existence")
        .to_string();
    assert!(
        err.contains("not yet implemented"),
        "expected 'not yet implemented' wording, got {err}"
    );
}

#[test]
fn derived_descriptor_rejects_empty_query_string() {
    let rt = runtime();
    let err = rt
        .execute_query(
            "CREATE METRIC product.dau \
             TYPE gauge ROLE kpi \
             SOURCE product_events \
             QUERY '' \
             WINDOW 1 DAY",
        )
        .expect_err("empty QUERY should fail")
        .to_string();
    assert!(
        err.contains("QUERY"),
        "expected QUERY validation error, got {err}"
    );
}
