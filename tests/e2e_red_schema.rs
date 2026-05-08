//! Runtime-backed virtual `red.*` schema tables.

use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_auth_identity, set_current_connection_id, set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn cleanup_scope() {
    clear_current_auth_identity();
    clear_current_tenant();
    clear_current_connection_id();
}

#[test]
fn select_from_red_collections_materializes_catalog_rows() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");

    let result = rt
        .execute_query("SELECT * FROM red.collections WHERE name = 'users'")
        .expect("red.collections select");

    assert_eq!(
        result.result.columns,
        vec![
            "name",
            "model",
            "schema_mode",
            "entities",
            "segments",
            "indices",
            "in_memory_bytes",
            "tenant_id"
        ]
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(row.get("name"), Some(&Value::text("users")));
    assert_eq!(row.get("model"), Some(&Value::text("table")));
    assert_eq!(row.get("schema_mode"), Some(&Value::text("strict")));
    assert_eq!(row.get("entities"), Some(&Value::UnsignedInteger(1)));
    assert!(matches!(row.get("indices"), Some(Value::Array(_))));
    assert!(matches!(
        row.get("in_memory_bytes"),
        Some(Value::UnsignedInteger(_))
    ));

    cleanup_scope();
}

#[test]
fn red_collections_requires_tenant_for_non_admin_identity() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE events (id INT)");
    set_current_connection_id(24401);
    set_current_auth_identity("alice".to_string(), Role::Read);

    let err = rt
        .execute_query("SELECT * FROM red.collections")
        .expect_err("tenant-less non-admin should be rejected")
        .to_string();
    assert!(err.contains("active tenant"), "error was: {err}");

    set_current_tenant("acme".to_string());
    let result = rt
        .execute_query("SELECT tenant_id FROM red.collections WHERE name = 'events'")
        .expect("tenant-scoped catalog read");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("tenant_id"),
        Some(&Value::text("acme"))
    );

    cleanup_scope();
}

#[test]
fn red_collections_admin_identity_bypasses_tenant_requirement() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE admin_visible (id INT)");
    set_current_connection_id(24402);
    set_current_auth_identity("root".to_string(), Role::Admin);

    let result = rt
        .execute_query("SELECT tenant_id FROM red.collections WHERE name = 'admin_visible'")
        .expect("admin catalog read");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("tenant_id"),
        Some(&Value::Null)
    );

    cleanup_scope();
}

#[test]
fn red_schema_dml_is_read_only() {
    cleanup_scope();
    let rt = runtime();
    for sql in [
        "INSERT INTO red.collections (name) VALUES ('x')",
        "UPDATE red.collections SET name = 'x'",
        "DELETE FROM red.collections WHERE name = 'x'",
    ] {
        let err = match rt.execute_query(sql) {
            Ok(_) => panic!("expected read-only error for {sql}"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("system schema is read-only"),
            "{sql} returned unexpected error: {err}"
        );
    }
    cleanup_scope();
}
