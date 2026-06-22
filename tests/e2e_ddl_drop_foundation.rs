use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::application::ExecuteQueryInput;
use reddb::auth::{AuthConfig, AuthStore};
use reddb::catalog::{CollectionModel, SchemaMode};
use reddb::physical::{CollectionContract, ContractOrigin};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{
    storage::{DeployProfile, StoragePackaging, StorageProfileSelection},
    QueryUseCases, RedDBOptions, RedDBRuntime,
};

#[allow(dead_code)]
mod support;

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn ddl_drop_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn unique_ident(prefix: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{unique}")
}

fn rt_with_vault(path: &Path) -> RedDBRuntime {
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(StorageProfileSelection {
            deploy_profile: DeployProfile::Embedded,
            packaging: StoragePackaging::OperationalDirectory,
            replica_count: 0,
            managed_backup: false,
            wal_retention: false,
        })
        .expect("operational storage profile should validate");
    let rt = RedDBRuntime::with_options(options).expect("runtime should open");
    let pager = Arc::clone(
        rt.db()
            .store()
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, TEST_CERTIFICATE)
            .expect("vault should open"),
    );
    rt.set_auth_store(auth);
    rt
}

fn text_field(row: &UnifiedRecord, field: &str) -> String {
    match row.get(field) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text field {field}, got {other:?}"),
    }
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    match QueryUseCases::new(rt).execute(ExecuteQueryInput {
        query: sql.to_string(),
    }) {
        Ok(_) => panic!("expected error for {sql}"),
        Err(err) => err.to_string(),
    }
}

fn register_collection(rt: &RedDBRuntime, name: &str, model: CollectionModel) {
    rt.db()
        .store()
        .create_collection(name)
        .unwrap_or_else(|err| panic!("create {name}: {err}"));
    rt.db()
        .save_collection_contract(CollectionContract {
            name: name.to_string(),
            declared_model: model,
            schema_mode: SchemaMode::Dynamic,
            origin: ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            default_ttl_ms: None,
            vector_dimension: None,
            vector_metric: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: None,
            timestamps_enabled: false,
            context_index_enabled: false,
            metrics_raw_retention_ms: None,
            metrics_rollup_policies: Vec::new(),
            metrics_tenant_identity: None,
            metrics_namespace: None,
            analytics_config: Vec::new(),
            append_only: false,
            subscriptions: Vec::new(),
            session_key: None,
            session_gap_ms: None,
            retention_duration_ms: None,
            analytical_storage: None,
        })
        .unwrap_or_else(|err| panic!("contract {name}: {err}"));
}

#[test]
fn typed_drop_removes_non_table_models() {
    let _guard = ddl_drop_test_lock().lock().unwrap();
    let path = support::temp_db_file("ddl-drop-typed-models");
    let rt = rt_with_vault(path.path());
    for (name, create_sql, drop_sql) in [
        {
            let name = unique_ident("identity");
            (
                name.clone(),
                format!("CREATE GRAPH {name}"),
                format!("DROP GRAPH {name}"),
            )
        },
        {
            let name = unique_ident("notes");
            (
                name.clone(),
                format!("CREATE VECTOR {name} DIM 3"),
                format!("DROP VECTOR {name}"),
            )
        },
        {
            let name = unique_ident("logs");
            (
                name.clone(),
                format!("CREATE DOCUMENT {name}"),
                format!("DROP DOCUMENT {name}"),
            )
        },
        {
            let name = unique_ident("settings");
            (
                name.clone(),
                format!("CREATE KV {name}"),
                format!("DROP KV {name}"),
            )
        },
        {
            let name = unique_ident("app_settings");
            (
                name.clone(),
                format!("CREATE CONFIG {name}"),
                format!("DROP CONFIG {name}"),
            )
        },
        {
            let name = unique_ident("secrets");
            (
                name.clone(),
                format!("CREATE VAULT {name}"),
                format!("DROP VAULT {name}"),
            )
        },
    ] {
        exec(&rt, &create_sql);
        exec(&rt, &drop_sql);
        assert!(rt.db().store().get_collection(&name).is_none(), "{name}");
        assert!(rt.db().collection_contract(&name).is_none(), "{name}");
    }
    drop(rt);
}

#[test]
fn drop_collection_dispatches_polymorphically_and_if_exists_is_idempotent() {
    let _guard = ddl_drop_test_lock().lock().unwrap();
    let rt = rt();
    let users = unique_ident("users");
    exec(&rt, &format!("CREATE TABLE {users} (id INT)"));
    exec(&rt, &format!("DROP COLLECTION {users}"));
    assert!(rt.db().store().get_collection(&users).is_none());

    exec(&rt, "DROP TABLE IF EXISTS missing_table");
    exec(&rt, "DROP COLLECTION IF EXISTS missing_collection");
}

#[test]
fn create_keyed_models_are_visible_in_typed_show_filters() {
    let _guard = ddl_drop_test_lock().lock().unwrap();
    let path = support::temp_db_file("ddl-drop-foundation-vault");
    let rt = rt_with_vault(path.path());
    let sessions = unique_ident("sessions");
    let app_settings = unique_ident("app_settings");
    let secrets = unique_ident("secrets");
    exec(&rt, &format!("CREATE KV {sessions}"));
    exec(&rt, &format!("CREATE CONFIG {app_settings}"));
    exec(&rt, &format!("CREATE VAULT {secrets}"));

    for (sql, expected_name, expected_model) in [
        ("SHOW KVS", sessions.as_str(), "kv"),
        ("SHOW CONFIGS", app_settings.as_str(), "config"),
        ("SHOW VAULTS", secrets.as_str(), "vault"),
    ] {
        let result = rt
            .execute_query(sql)
            .unwrap_or_else(|err| panic!("{sql}: {err}"));
        assert!(
            result.result.records.iter().any(|row| {
                text_field(row, "name") == expected_name
                    && text_field(row, "model") == expected_model
            }),
            "{sql} should include {expected_name}"
        );
        assert!(
            result
                .result
                .records
                .iter()
                .all(|row| text_field(row, "model") == expected_model),
            "{sql} should only return {expected_model} models"
        );
    }
    drop(rt);
}

#[test]
fn drop_model_mismatch_and_system_schema_are_rejected() {
    let _guard = ddl_drop_test_lock().lock().unwrap();
    let rt = rt();
    let jobs = unique_ident("jobs");
    let app_settings = unique_ident("app_settings");
    exec(&rt, &format!("CREATE QUEUE {jobs}"));

    let err = exec_err(&rt, &format!("DROP TABLE {jobs}"));
    assert!(
        err.contains("model mismatch: expected table, got queue"),
        "unexpected error: {err}"
    );

    exec(&rt, &format!("CREATE CONFIG {app_settings}"));
    let err = exec_err(&rt, &format!("DROP KV {app_settings}"));
    assert!(
        err.contains("INVALID_OPERATION")
            && err.contains("model mismatch: expected kv, got config"),
        "unexpected error: {err}"
    );

    let err = exec_err(&rt, "DROP COLLECTION red.collections");
    assert!(
        err.contains("system schema is read-only"),
        "unexpected error: {err}"
    );
}
