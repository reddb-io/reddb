#[path = "../../support/mod.rs"]
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

fn float(row: &reddb::storage::query::UnifiedRecord, field: &str) -> f64 {
    match row.get(field) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected {field} float, got {other:?}"),
    }
}

fn integer(row: &reddb::storage::query::UnifiedRecord, field: &str) -> i64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected {field} integer, got {other:?}"),
    }
}

#[test]
fn create_slo_descriptor_persists_and_reads_from_catalog() {
    let path = PersistentDbPath::new("slo_descriptor_catalog");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRIC infra.api.success_ratio TYPE ratio ROLE sli",
    );
    exec(
        &rt,
        "CREATE SLO infra.api.availability ON infra.api.success_ratio TARGET 0.999 WINDOW 30 DAYS",
    );

    let listed = exec(
        &rt,
        "SELECT path, metric, target, window_ms FROM red.analytics.slos \
         WHERE path = 'infra.api.availability'",
    );
    assert_eq!(listed.result.records.len(), 1);
    let row = &listed.result.records[0];
    assert_eq!(text(row, "path"), "infra.api.availability");
    assert_eq!(text(row, "metric"), "infra.api.success_ratio");
    assert!((float(row, "target") - 0.999).abs() < f64::EPSILON);
    // 30 days in ms.
    assert_eq!(integer(row, "window_ms"), 30 * 86_400_000);

    // Durable through checkpoint + reopen — the catalog write goes
    // through `red_config` so it inherits the same WAL behaviour the
    // metric descriptor catalog has.
    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT path, metric, target, window_ms FROM red.analytics.slos \
         WHERE path = 'infra.api.availability'",
    );
    assert_eq!(persisted.result.records.len(), 1);
    let row = &persisted.result.records[0];
    assert_eq!(text(row, "path"), "infra.api.availability");
    assert_eq!(text(row, "metric"), "infra.api.success_ratio");
    assert!((float(row, "target") - 0.999).abs() < f64::EPSILON);
    assert_eq!(integer(row, "window_ms"), 30 * 86_400_000);
}

#[test]
fn create_slo_descriptor_rejects_unknown_metric() {
    let rt = runtime();
    let err = rt
        .execute_query(
            "CREATE SLO infra.api.availability ON infra.api.missing TARGET 0.99 WINDOW 7 DAYS",
        )
        .expect_err("unknown metric should fail")
        .to_string();
    assert!(
        err.contains("does not exist"),
        "expected 'does not exist' error, got {err}"
    );
    assert!(
        err.contains("infra.api.missing"),
        "expected error to name the missing metric, got {err}"
    );
}

#[test]
fn create_slo_descriptor_rejects_non_sli_metric() {
    let rt = runtime();
    // Metric exists but its role is operational, not sli — the SLO
    // contract is only meaningful over SLI metrics, so this is a hard
    // error rather than a silent cast.
    exec(
        &rt,
        "CREATE METRIC infra.api.qps TYPE counter ROLE operational",
    );
    let err = rt
        .execute_query("CREATE SLO infra.api.qps_slo ON infra.api.qps TARGET 0.99 WINDOW 7 DAYS")
        .expect_err("non-sli metric should fail")
        .to_string();
    assert!(
        err.contains("expected 'sli'"),
        "expected role validation error, got {err}"
    );
    assert!(
        err.contains("operational"),
        "expected error to surface the current role, got {err}"
    );
}

#[test]
fn create_slo_descriptor_rejects_invalid_target() {
    let rt = runtime();
    exec(&rt, "CREATE METRIC infra.api.success TYPE ratio ROLE sli");

    for (sql, label) in [
        (
            "CREATE SLO a.b ON infra.api.success TARGET 0 WINDOW 7 DAYS",
            "target=0",
        ),
        (
            "CREATE SLO a.b ON infra.api.success TARGET 1.5 WINDOW 7 DAYS",
            "target>1",
        ),
    ] {
        let err = rt
            .execute_query(sql)
            .err()
            .unwrap_or_else(|| panic!("{label} should have failed"))
            .to_string();
        assert!(
            err.contains("invalid SLO target"),
            "{label}: expected 'invalid SLO target' error, got {err}"
        );
    }
}

#[test]
fn create_slo_descriptor_rejects_duplicate_path() {
    let rt = runtime();
    exec(&rt, "CREATE METRIC infra.api.success TYPE ratio ROLE sli");
    exec(
        &rt,
        "CREATE SLO infra.api.availability ON infra.api.success TARGET 0.99 WINDOW 7 DAYS",
    );
    let err = rt
        .execute_query(
            "CREATE SLO infra.api.availability ON infra.api.success TARGET 0.95 WINDOW 7 DAYS",
        )
        .expect_err("duplicate SLO should fail")
        .to_string();
    assert!(
        err.contains("already exists"),
        "expected 'already exists' error, got {err}"
    );
}

#[test]
fn create_slo_descriptor_rejects_invalid_path_shape() {
    let rt = runtime();
    exec(&rt, "CREATE METRIC infra.api.success TYPE ratio ROLE sli");
    // Single-segment path (no dot) should fail like CREATE METRIC.
    let err = rt
        .execute_query("CREATE SLO availability ON infra.api.success TARGET 0.99 WINDOW 7 DAYS")
        .expect_err("single-segment SLO path should fail")
        .to_string();
    assert!(
        err.contains("SLO descriptor"),
        "expected SLO path validation error, got {err}"
    );
}
