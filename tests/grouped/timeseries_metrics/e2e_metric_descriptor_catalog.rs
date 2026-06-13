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

#[test]
fn create_metric_descriptor_persists_and_reads_from_catalog() {
    let path = PersistentDbPath::new("metric_descriptor_catalog");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRIC infra.database.cpu.usage TYPE gauge ROLE operational",
    );

    let listed = exec(
        &rt,
        "SELECT path, kind, role FROM red.analytics.metrics WHERE path = 'infra.database.cpu.usage'",
    );
    assert_eq!(listed.result.records.len(), 1);
    let row = &listed.result.records[0];
    assert_eq!(text(row, "path"), "infra.database.cpu.usage");
    assert_eq!(text(row, "kind"), "gauge");
    assert_eq!(text(row, "role"), "operational");

    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT path, kind, role FROM red.analytics.metrics WHERE path = 'infra.database.cpu.usage'",
    );
    assert_eq!(persisted.result.records.len(), 1);
    let row = &persisted.result.records[0];
    assert_eq!(text(row, "path"), "infra.database.cpu.usage");
    assert_eq!(text(row, "kind"), "gauge");
    assert_eq!(text(row, "role"), "operational");
}

#[test]
fn alter_metric_descriptor_updates_role_and_persists() {
    let path = PersistentDbPath::new("metric_descriptor_alter_role");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRIC infra.api.latency TYPE histogram ROLE operational",
    );

    let initial = exec(
        &rt,
        "SELECT role FROM red.analytics.metrics WHERE path = 'infra.api.latency'",
    );
    assert_eq!(text(&initial.result.records[0], "role"), "operational");

    exec(&rt, "ALTER METRIC infra.api.latency SET ROLE sli");

    let updated = exec(
        &rt,
        "SELECT path, kind, role FROM red.analytics.metrics WHERE path = 'infra.api.latency'",
    );
    assert_eq!(updated.result.records.len(), 1);
    let row = &updated.result.records[0];
    // Path and kind survive a role update unchanged.
    assert_eq!(text(row, "path"), "infra.api.latency");
    assert_eq!(text(row, "kind"), "histogram");
    assert_eq!(text(row, "role"), "sli");

    // The update is durable through a checkpoint/reopen cycle —
    // it landed in red_config like every other WAL-backed catalog write.
    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT role FROM red.analytics.metrics WHERE path = 'infra.api.latency'",
    );
    assert_eq!(text(&persisted.result.records[0], "role"), "sli");
}

#[test]
fn alter_metric_descriptor_rejects_immutable_kind() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE METRIC infra.db.qps TYPE counter ROLE operational",
    );

    let err = rt
        .execute_query("ALTER METRIC infra.db.qps SET KIND gauge")
        .expect_err("kind change should fail")
        .to_string();
    assert!(
        err.contains("'kind' cannot be changed"),
        "expected explicit 'kind cannot be changed' error, got {err}"
    );
    assert!(
        err.contains("gauge"),
        "expected error to surface the attempted value, got {err}"
    );

    // TYPE is the alias the parser accepts on CREATE — make sure it is
    // also rejected at ALTER time, not silently ignored.
    let err = rt
        .execute_query("ALTER METRIC infra.db.qps SET TYPE gauge")
        .expect_err("type change should fail")
        .to_string();
    assert!(
        err.contains("'kind' cannot be changed"),
        "expected SET TYPE to map onto the kind immutability rule, got {err}"
    );

    // The descriptor still reads back unchanged.
    let row_count = exec(
        &rt,
        "SELECT kind FROM red.analytics.metrics WHERE path = 'infra.db.qps'",
    );
    assert_eq!(text(&row_count.result.records[0], "kind"), "counter");
}

#[test]
fn alter_metric_descriptor_rejects_path_rename() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE METRIC infra.cache.hit_rate TYPE ratio ROLE kpi",
    );

    let err = rt
        .execute_query("ALTER METRIC infra.cache.hit_rate SET PATH infra.cache.hits")
        .expect_err("path rename should fail")
        .to_string();
    assert!(
        err.contains("'path' cannot be changed"),
        "expected explicit path-immutability error, got {err}"
    );
}

#[test]
fn alter_metric_descriptor_rejects_unknown_descriptor() {
    let rt = runtime();
    let err = rt
        .execute_query("ALTER METRIC infra.never.declared SET ROLE kpi")
        .expect_err("unknown descriptor should fail")
        .to_string();
    assert!(
        err.contains("does not exist"),
        "expected 'does not exist' error, got {err}"
    );
}

#[test]
fn alter_metric_descriptor_rejects_invalid_role() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE METRIC infra.queue.depth TYPE gauge ROLE operational",
    );
    let err = rt
        .execute_query("ALTER METRIC infra.queue.depth SET ROLE bogus")
        .expect_err("invalid role should fail")
        .to_string();
    assert!(
        err.contains("invalid metric descriptor role"),
        "expected role validation error, got {err}"
    );
}

#[test]
fn create_metric_descriptor_rejects_invalid_shape() {
    let rt = runtime();

    for sql in [
        "CREATE METRIC cpu TYPE gauge ROLE operational",
        "CREATE METRIC infra.database.cpu.usage TYPE timer ROLE operational",
        "CREATE METRIC infra.database.cpu.latency TYPE histogram ROLE random",
    ] {
        let err = rt
            .execute_query(sql)
            .expect_err(&format!("{sql} should fail"))
            .to_string();
        assert!(
            err.contains("metric descriptor"),
            "expected clear metric descriptor error for {sql}, got {err}"
        );
    }
}
