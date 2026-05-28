//! Issue #747 — typed `red.*` relations for timeseries and metrics.
//!
//! Pins the contract that the Red UI chart/KPI toolbars depend on:
//!
//! 1. `red.timeseries` exposes timeseries-shaped columns
//!    (`name, schema_mode, is_hypertable, time_column,
//!    chunk_interval_ms, chunk_count, retention_ms, session_key,
//!    session_gap_ms, row_count, oldest_ts_ms, newest_ts_ms,
//!    in_memory_bytes, on_disk_bytes, tenant_id, internal`) — no
//!    table/document/queue noise.
//! 2. `red.timeseries` reports `is_hypertable = true` and populates
//!    `time_column` / `chunk_interval_ms` for collections created via
//!    `CREATE HYPERTABLE`; standalone `CREATE TIMESERIES` reports
//!    `is_hypertable = false` and `NULL` for those columns.
//! 3. `red.metrics` exposes metric-descriptor-shaped columns
//!    (`path, kind, role, labels, unit, retention_ms,
//!    supports_prometheus_query, created_at_ms`) and `labels` / `unit`
//!    / `retention_ms` come back `NULL` until the descriptor catalog
//!    grows those fields.
//! 4. Each typed relation only returns rows for its model — `red.
//!    timeseries` skips Metrics collections and vice versa.
//! 5. Tenant scope is respected for `red.timeseries` like the other
//!    `red.*` collection surfaces.

use std::collections::HashSet;

use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBOptions;

const TIMESERIES_COLUMNS: [&str; 16] = [
    "name",
    "schema_mode",
    "is_hypertable",
    "time_column",
    "chunk_interval_ms",
    "chunk_count",
    "retention_ms",
    "session_key",
    "session_gap_ms",
    "row_count",
    "oldest_ts_ms",
    "newest_ts_ms",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
];

const METRICS_COLUMNS: [&str; 8] = [
    "path",
    "kind",
    "role",
    "labels",
    "unit",
    "retention_ms",
    "supports_prometheus_query",
    "created_at_ms",
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

fn find_row<'a>(
    records: &'a [UnifiedRecord],
    field: &str,
    name: &str,
) -> Option<&'a UnifiedRecord> {
    records.iter().find(|record| match record.get(field) {
        Some(Value::Text(value)) => value.as_ref() == name,
        _ => false,
    })
}

fn boolean(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column `{column}`, got {other:?}"),
    }
}

fn names(records: &[UnifiedRecord], field: &str) -> HashSet<String> {
    records
        .iter()
        .filter_map(|record| match record.get(field) {
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
fn red_timeseries_exposes_timeseries_shaped_columns_for_plain_timeseries() {
    let rt = open_runtime();
    exec(&rt, "CREATE TIMESERIES events RETENTION 7 d");

    let result = select(&rt, "SELECT * FROM red.timeseries");
    assert_columns(&result.result.columns, &TIMESERIES_COLUMNS);

    let row = find_row(&result.result.records, "name", "events").expect("events row");
    assert!(
        !boolean(row, "is_hypertable"),
        "plain CREATE TIMESERIES is not a hypertable"
    );
    assert!(matches!(row.get("time_column"), Some(Value::Null)));
    assert!(matches!(row.get("chunk_interval_ms"), Some(Value::Null)));
    assert!(matches!(row.get("oldest_ts_ms"), Some(Value::Null)));
    assert!(matches!(row.get("newest_ts_ms"), Some(Value::Null)));
    // retention 7 days → 604_800_000 ms.
    assert_eq!(
        row.get("retention_ms"),
        Some(&Value::UnsignedInteger(7 * 86_400_000))
    );
    assert!(!boolean(row, "internal"));
}

#[test]
fn red_timeseries_reports_hypertable_chunk_metadata() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'",
    );

    let result = select(&rt, "SELECT * FROM red.timeseries");
    let row = find_row(&result.result.records, "name", "metrics").expect("metrics row");
    assert!(
        boolean(row, "is_hypertable"),
        "CREATE HYPERTABLE rows must report is_hypertable = true"
    );
    assert_eq!(row.get("time_column"), Some(&Value::text("ts")));
    // 1 day chunk = 86_400_000 ms.
    assert_eq!(
        row.get("chunk_interval_ms"),
        Some(&Value::UnsignedInteger(86_400_000))
    );
    // No INSERTs landed yet → no chunks allocated, so oldest/newest
    // are NULL and chunk_count is 0.
    assert_eq!(row.get("chunk_count"), Some(&Value::UnsignedInteger(0)));
    assert!(matches!(row.get("oldest_ts_ms"), Some(Value::Null)));
    assert!(matches!(row.get("newest_ts_ms"), Some(Value::Null)));
}

#[test]
fn red_timeseries_skips_other_models() {
    let rt = open_runtime();
    exec(&rt, "CREATE TIMESERIES samples RETENTION 1 d");
    exec(&rt, "CREATE TABLE plain_table (id INT)");
    exec(&rt, "CREATE DOCUMENT plain_docs");
    exec(&rt, "CREATE METRICS plain_metrics RETENTION 30 d");

    let result = select(&rt, "SELECT name FROM red.timeseries");
    let names = names(&result.result.records, "name");
    assert!(
        names.contains("samples"),
        "samples missing from red.timeseries: {names:?}"
    );
    assert!(
        !names.contains("plain_table"),
        "red.timeseries must not include tables: {names:?}"
    );
    assert!(
        !names.contains("plain_docs"),
        "red.timeseries must not include documents: {names:?}"
    );
    assert!(
        !names.contains("plain_metrics"),
        "red.timeseries must not include metric collections: {names:?}"
    );
}

#[test]
fn red_timeseries_respects_tenant_scope() {
    let rt = open_runtime();
    exec(&rt, "SET TENANT 'acme'");
    exec(&rt, "CREATE TIMESERIES acme_events RETENTION 1 d");

    exec(&rt, "SET TENANT 'globex'");
    exec(&rt, "CREATE TIMESERIES globex_events RETENTION 1 d");

    let visible = names(
        &select(&rt, "SELECT name FROM red.timeseries")
            .result
            .records,
        "name",
    );
    assert!(
        visible.contains("globex_events"),
        "globex sees its own timeseries: {visible:?}"
    );
    assert!(
        !visible.contains("acme_events"),
        "globex must not see acme's timeseries: {visible:?}"
    );

    exec(&rt, "SET TENANT NULL");
    let admin = names(
        &select(&rt, "SELECT name FROM red.timeseries")
            .result
            .records,
        "name",
    );
    assert!(admin.contains("acme_events"));
    assert!(admin.contains("globex_events"));
}

#[test]
fn red_metrics_exposes_metric_descriptor_shaped_columns() {
    let rt = open_runtime();
    exec(&rt, "CREATE METRIC infra.cpu.usage TYPE gauge ROLE metric");

    let result = select(&rt, "SELECT * FROM red.metrics");
    assert_columns(&result.result.columns, &METRICS_COLUMNS);

    let row =
        find_row(&result.result.records, "path", "infra.cpu.usage").expect("infra.cpu.usage row");
    assert_eq!(row.get("kind"), Some(&Value::text("gauge")));
    assert_eq!(row.get("role"), Some(&Value::text("metric")));
    // Labels / unit / retention_ms aren't tracked yet; the schema
    // pins them as NULL so the UI can render them stably.
    assert!(matches!(row.get("labels"), Some(Value::Null)));
    assert!(matches!(row.get("unit"), Some(Value::Null)));
    assert!(matches!(row.get("retention_ms"), Some(Value::Null)));
    assert!(
        boolean(row, "supports_prometheus_query"),
        "metric descriptors are queryable via the Prometheus adapter"
    );
    assert!(matches!(
        row.get("created_at_ms"),
        Some(Value::TimestampMs(_))
    ));
}

#[test]
fn red_metrics_skips_collection_rows() {
    let rt = open_runtime();
    // A Metrics-model *collection* is not a metric descriptor — the
    // typed relation only enumerates descriptors registered via
    // `CREATE METRIC`.
    exec(&rt, "CREATE METRICS app_metrics RETENTION 7 d");
    exec(
        &rt,
        "CREATE METRIC app.requests.total TYPE counter ROLE operational",
    );

    let result = select(&rt, "SELECT path FROM red.metrics");
    let paths = names(&result.result.records, "path");
    assert!(
        paths.contains("app.requests.total"),
        "descriptor missing: {paths:?}"
    );
    assert!(
        !paths.contains("app_metrics"),
        "red.metrics must not include metric collections, only descriptors: {paths:?}"
    );
}

#[test]
fn red_metrics_does_not_collide_with_red_analytics_metrics_rewrite() {
    // The `red.metrics` rewrite needle is a prefix substring of
    // `red.analytics.metrics`. Pin that both surfaces resolve to
    // their own contracts so the typed projection does not steal
    // queries aimed at the analytics descriptor catalog.
    let rt = open_runtime();
    exec(&rt, "CREATE METRIC infra.disk.used TYPE gauge ROLE metric");

    let typed = select(&rt, "SELECT * FROM red.metrics");
    assert_columns(&typed.result.columns, &METRICS_COLUMNS);

    let analytics = select(&rt, "SELECT * FROM red.analytics.metrics");
    // `red.analytics.metrics` is the descriptor catalog projection
    // shipped in #784 — its column contract is `path, kind, role,
    // created_at`. We don't need to assert the full set here; the
    // signal we care about is that the relation resolved at all
    // (i.e. wasn't shadowed by the new `red.metrics` rewrite) and
    // doesn't carry the typed-relation `supports_prometheus_query`
    // column.
    let columns: HashSet<&str> = analytics
        .result
        .columns
        .iter()
        .map(String::as_str)
        .collect();
    assert!(
        columns.contains("path") && columns.contains("kind"),
        "red.analytics.metrics must still resolve to the descriptor catalog: {columns:?}"
    );
    assert!(
        !columns.contains("supports_prometheus_query"),
        "red.analytics.metrics must not pick up red.metrics columns: {columns:?}"
    );
}
