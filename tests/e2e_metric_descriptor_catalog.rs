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
