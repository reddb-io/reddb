//! Issue #792 — Analytics v0 end-to-end smoke: raw collections → metric
//! descriptors → SLO.
//!
//! This smoke walks the whole v0 catalog story in one runtime, exactly as a
//! user adopting Analytics v0 would, and pins that it is *descriptor/catalog*
//! behaviour only — no derived-metric output is ever computed:
//!
//!   1. Ordinary RedDB collections hold the raw data (a plain `events` table
//!      and an `http_requests` table). These are normal collections, writable
//!      and readable like any other.
//!   2. An event-shaped *analytics source* is registered over the normal
//!      `events` collection (`CREATE ANALYTICS SOURCE ... ON events`). The
//!      source is a profile over the existing collection — it does not create
//!      a new raw collection of its own.
//!   3. Three metric descriptors are declared, one per v0 role: `operational`
//!      (raw infra signal), `kpi` (derived business signal over the event
//!      source), and `sli` (derived reliability signal over the requests
//!      source). The derived descriptors carry SOURCE/QUERY/WINDOW metadata
//!      only — the inputs a future execution layer would consume.
//!   4. An SLO descriptor is declared over the SLI metric.
//!   5. Every descriptor is read back through its catalog virtual table
//!      (`red.analytics.sources`, `red.analytics.metrics`, `red.analytics.slos`)
//!      and the whole catalog survives a checkpoint/reopen cycle — proving the
//!      descriptors are queryable and durable *without* executing any metric
//!      output.
//!
//! A second test pins the v0 boundary directly: the descriptors stay
//! catalog-queryable even though reading a metric's *output value*
//! (`READ METRIC`) is explicitly unsupported in v0. Querying the catalog must
//! never require metric-output execution.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use support::{checkpoint_and_reopen, PersistentDbPath};

fn exec(rt: &reddb::RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn float(row: &reddb::storage::query::UnifiedRecord, field: &str) -> f64 {
    match row.get(field) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected {field} float, got {other:?} in {row:?}"),
    }
}

fn integer(row: &reddb::storage::query::UnifiedRecord, field: &str) -> i64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected {field} integer, got {other:?} in {row:?}"),
    }
}

/// Build the full v0 catalog story over a fresh runtime: raw collections, an
/// event-shaped source, the three metric roles, and an SLO over the SLI metric.
fn seed_catalog(rt: &reddb::RedDBRuntime) {
    // 1. Ordinary raw collections — nothing analytics-specific about them.
    exec(
        rt,
        "CREATE TABLE events (ts INTEGER, event_name TEXT, actor_id TEXT, session_id TEXT, props TEXT)",
    );
    exec(
        rt,
        "CREATE TABLE http_requests (received_at INTEGER, route TEXT, status INTEGER)",
    );
    // Raw data flows into the plain collection as usual.
    exec(
        rt,
        "INSERT INTO events (ts, event_name, actor_id, session_id, props) \
         VALUES (1, 'signup', 'user-1', 'sess-1', '{}')",
    );
    exec(
        rt,
        "INSERT INTO http_requests (received_at, route, status) VALUES (1, '/checkout', 200)",
    );

    // 2. An event-shaped source registered over the normal `events` collection.
    exec(
        rt,
        "CREATE ANALYTICS SOURCE product_events ON events \
         TIME FIELD ts EVENT FIELD event_name ACTOR FIELD actor_id \
         SESSION FIELD session_id PROPERTIES FIELD props",
    );

    // 3. The three metric-descriptor roles.
    //    operational: a raw infra signal, no derived inputs.
    exec(
        rt,
        "CREATE METRIC infra.api.qps TYPE counter ROLE operational",
    );
    //    kpi: a derived business signal over the event source.
    exec(
        rt,
        "CREATE METRIC product.daily_active_users \
         TYPE gauge ROLE kpi \
         SOURCE product_events \
         QUERY 'count_distinct(actor_id)' \
         WINDOW 1 DAY",
    );
    //    sli: a derived reliability signal over the requests source.
    exec(
        rt,
        "CREATE METRIC infra.api.success_ratio \
         TYPE ratio ROLE sli \
         SOURCE http_requests \
         QUERY 'sum(status < 500) / count(*)' \
         WINDOW 5 MINUTES \
         TIME_FIELD received_at",
    );

    // 4. An SLO over the SLI metric.
    exec(
        rt,
        "CREATE SLO infra.api.availability ON infra.api.success_ratio TARGET 0.999 WINDOW 30 DAYS",
    );
}

/// Assert the whole catalog reads back correctly. Runs against the live runtime
/// and again after a checkpoint/reopen, so the same assertions prove both
/// queryability and durability.
fn assert_catalog_visible(rt: &reddb::RedDBRuntime) {
    // The event-shaped source projects over the raw collection.
    let source = exec(
        rt,
        "SELECT name, collection, time_field, event_field, actor_field \
         FROM red.analytics.sources WHERE name = 'product_events'",
    );
    assert_eq!(source.result.records.len(), 1);
    let row = &source.result.records[0];
    assert_eq!(text(row, "name"), "product_events");
    assert_eq!(text(row, "collection"), "events");
    assert_eq!(text(row, "time_field"), "ts");
    assert_eq!(text(row, "event_field"), "event_name");
    assert_eq!(text(row, "actor_field"), "actor_id");

    // All three metric roles are present and read back with their roles intact.
    let metrics = exec(
        rt,
        "SELECT path, kind, role FROM red.analytics.metrics WHERE path STARTS WITH 'infra' \
         OR path STARTS WITH 'product'",
    );
    let mut roles: Vec<(String, String, String)> = metrics
        .result
        .records
        .iter()
        .map(|r| {
            (
                text(r, "path").to_string(),
                text(r, "kind").to_string(),
                text(r, "role").to_string(),
            )
        })
        .collect();
    roles.sort();
    assert_eq!(
        roles,
        vec![
            (
                "infra.api.qps".to_string(),
                "counter".to_string(),
                "operational".to_string(),
            ),
            (
                "infra.api.success_ratio".to_string(),
                "ratio".to_string(),
                "sli".to_string(),
            ),
            (
                "product.daily_active_users".to_string(),
                "gauge".to_string(),
                "kpi".to_string(),
            ),
        ],
        "operational, KPI and SLI descriptors must all be catalog-visible"
    );

    // The derived KPI descriptor carries its SOURCE/QUERY/WINDOW metadata —
    // the inputs only, never an executed value.
    let kpi = exec(
        rt,
        "SELECT source, query, window_ms FROM red.analytics.metrics \
         WHERE path = 'product.daily_active_users'",
    );
    let row = &kpi.result.records[0];
    assert_eq!(text(row, "source"), "product_events");
    assert_eq!(text(row, "query"), "count_distinct(actor_id)");
    assert_eq!(integer(row, "window_ms"), 86_400_000);

    // The SLO reads back over its SLI metric.
    let slo = exec(
        rt,
        "SELECT path, metric, target, window_ms FROM red.analytics.slos \
         WHERE path = 'infra.api.availability'",
    );
    assert_eq!(slo.result.records.len(), 1);
    let row = &slo.result.records[0];
    assert_eq!(text(row, "path"), "infra.api.availability");
    assert_eq!(text(row, "metric"), "infra.api.success_ratio");
    assert!((float(row, "target") - 0.999).abs() < f64::EPSILON);
    assert_eq!(integer(row, "window_ms"), 30 * 86_400_000);
}

#[test]
fn analytics_v0_raw_collection_to_metric_to_slo_smoke() {
    let path = PersistentDbPath::new("analytics_v0_smoke");
    let rt = path.open_runtime();

    seed_catalog(&rt);

    // The raw collections behind the catalog stay ordinary, writable
    // collections — registering a source/metric does not turn them into
    // something else.
    let raw = exec(&rt, "SELECT event_name FROM events");
    assert_eq!(raw.result.records.len(), 1);
    assert_eq!(text(&raw.result.records[0], "event_name"), "signup");

    // Queryable now …
    assert_catalog_visible(&rt);

    // … and durable across a checkpoint/reopen, with no metric output ever
    // executed in between.
    let reopened = checkpoint_and_reopen(&path, rt);
    assert_catalog_visible(&reopened);
}

#[test]
fn analytics_v0_descriptors_query_without_metric_output_execution() {
    let rt = reddb::RedDBRuntime::in_memory().expect("runtime");
    seed_catalog(&rt);

    // The catalog is fully queryable …
    assert_catalog_visible(&rt);

    // … while reading a derived metric's *output value* is explicitly the v0
    // boundary. Catalog visibility must not depend on that execution path.
    let err = rt
        .execute_query("READ METRIC product.daily_active_users")
        .expect_err("metric output read must remain unsupported in Analytics v0")
        .to_string();
    assert!(
        err.contains("not yet implemented"),
        "expected v0 'not yet implemented' boundary, got {err}"
    );
    assert!(
        err.contains("Analytics v0"),
        "expected the v0 boundary to be named, got {err}"
    );

    // The descriptor is still catalog-visible after the unsupported output
    // read — the boundary is on execution, not on the descriptor.
    let still_there = exec(
        &rt,
        "SELECT path FROM red.analytics.metrics WHERE path = 'product.daily_active_users'",
    );
    assert_eq!(still_there.result.records.len(), 1);
}
