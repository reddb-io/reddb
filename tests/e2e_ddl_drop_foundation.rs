use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::application::ExecuteQueryInput;
use reddb::auth::{AuthConfig, AuthStore};
use reddb::catalog::{CollectionModel, SchemaMode};
use reddb::physical::{CollectionContract, ContractOrigin};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn temp_db_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{name}_{unique}.rdb"))
}

fn cleanup_related(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(stem) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Some(name) = entry_path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == stem || name.starts_with(&format!("{stem}-")) {
                let _ = std::fs::remove_file(&entry_path);
                let _ = std::fs::remove_dir_all(&entry_path);
            }
        }
    }
}

fn rt_with_vault(path: &Path) -> RedDBRuntime {
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("runtime should open");
    let pager = Arc::clone(
        rt.db()
            .store()
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault(AuthConfig::default(), pager, Some("ddl-drop-foundation"))
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
            append_only: false,
            subscriptions: Vec::new(),
            session_key: None,
            session_gap_ms: None,
        })
        .unwrap_or_else(|err| panic!("contract {name}: {err}"));
}

#[test]
fn typed_drop_removes_non_table_models() {
    let rt = rt();
    for (name, model, sql) in [
        ("identity", CollectionModel::Graph, "DROP GRAPH identity"),
        ("notes", CollectionModel::Vector, "DROP VECTOR notes"),
        ("logs", CollectionModel::Document, "DROP DOCUMENT logs"),
        ("settings", CollectionModel::Kv, "DROP KV settings"),
        (
            "app_settings",
            CollectionModel::Config,
            "DROP CONFIG app_settings",
        ),
        ("secrets", CollectionModel::Vault, "DROP VAULT secrets"),
    ] {
        register_collection(&rt, name, model);
        exec(&rt, sql);
        assert!(rt.db().store().get_collection(name).is_none(), "{name}");
        assert!(rt.db().collection_contract(name).is_none(), "{name}");
    }
}

#[test]
fn drop_collection_dispatches_polymorphically_and_if_exists_is_idempotent() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "DROP COLLECTION users");
    assert!(rt.db().store().get_collection("users").is_none());

    exec(&rt, "DROP TABLE IF EXISTS missing_table");
    exec(&rt, "DROP COLLECTION IF EXISTS missing_collection");
}

#[test]
fn create_keyed_models_are_visible_in_typed_show_filters() {
    let path = temp_db_path("ddl_drop_foundation_vault");
    cleanup_related(&path);
    let rt = rt_with_vault(&path);
    exec(&rt, "CREATE KV sessions");
    exec(&rt, "CREATE CONFIG app_settings");
    exec(&rt, "CREATE VAULT secrets");

    for (sql, expected_name, expected_model) in [
        ("SHOW KVS", "sessions", "kv"),
        ("SHOW CONFIGS", "app_settings", "config"),
        ("SHOW VAULTS", "secrets", "vault"),
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
    cleanup_related(&path);
}

#[test]
fn drop_model_mismatch_and_system_schema_are_rejected() {
    let rt = rt();
    exec(&rt, "CREATE QUEUE jobs");

    let err = exec_err(&rt, "DROP TABLE jobs");
    assert!(
        err.contains("model mismatch: expected table, got queue"),
        "unexpected error: {err}"
    );

    exec(&rt, "CREATE CONFIG app_settings");
    let err = exec_err(&rt, "DROP KV app_settings");
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
