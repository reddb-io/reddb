use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::application::ExecuteQueryInput;
use reddb::auth::{AuthConfig, AuthStore};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};

#[allow(dead_code)]
mod support;

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
