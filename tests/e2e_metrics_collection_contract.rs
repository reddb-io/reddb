mod support;

use reddb::catalog::{CollectionModel, SchemaMode};
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
fn create_metrics_persists_minimal_contract_and_introspection() {
    let path = PersistentDbPath::new("metrics_collection_contract");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE METRICS app_metrics RETENTION 30 d TENANT BY (tenant_id)",
    );

    let contract = rt
        .db()
        .collection_contract("app_metrics")
        .expect("metrics contract should exist");
    assert_eq!(contract.declared_model, CollectionModel::Metrics);
    assert_eq!(contract.schema_mode, SchemaMode::SemiStructured);
    assert_eq!(contract.default_ttl_ms, Some(30 * 86_400_000));
    assert_eq!(contract.metrics_raw_retention_ms, Some(30 * 86_400_000));
    assert_eq!(
        contract.metrics_tenant_identity.as_deref(),
        Some("tenant_id")
    );
    assert_eq!(contract.metrics_namespace.as_deref(), Some("default"));
    assert!(contract.append_only);

    let listed = exec(
        &rt,
        "SELECT name, model FROM red.collections WHERE name = 'app_metrics'",
    );
    assert_eq!(listed.result.records.len(), 1);
    assert_eq!(text(&listed.result.records[0], "model"), "metrics");

    let reopened = checkpoint_and_reopen(&path, rt);
    let reopened_contract = reopened
        .db()
        .collection_contract("app_metrics")
        .expect("metrics contract should survive reopen");
    assert_eq!(reopened_contract.declared_model, CollectionModel::Metrics);
    assert_eq!(
        reopened_contract.metrics_raw_retention_ms,
        Some(30 * 86_400_000)
    );
    assert_eq!(
        reopened_contract.metrics_tenant_identity.as_deref(),
        Some("tenant_id")
    );
    assert_eq!(
        reopened_contract.metrics_namespace.as_deref(),
        Some("default")
    );
}

#[test]
fn create_metrics_duplicate_drop_and_truncate_use_typed_contract() {
    let rt = runtime();
    exec(&rt, "CREATE METRICS app_metrics RETENTION 7 d");

    let duplicate = rt.execute_query("CREATE METRICS app_metrics");
    assert!(duplicate.is_err(), "duplicate create should fail");

    exec(&rt, "CREATE METRICS IF NOT EXISTS app_metrics");
    exec(&rt, "TRUNCATE METRICS app_metrics");

    exec(&rt, "CREATE TABLE ordinary (id INT)");
    let wrong_drop = rt.execute_query("DROP METRICS ordinary");
    assert!(wrong_drop.is_err(), "DROP METRICS must not drop a table");

    exec(&rt, "DROP METRICS app_metrics");
    assert!(rt.db().collection_contract("app_metrics").is_none());
}
