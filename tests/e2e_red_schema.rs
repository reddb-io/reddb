//! Runtime-backed virtual `red.*` schema tables.

use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_auth_identity, set_current_connection_id, set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

const INDEX_COLUMNS: [&str; 10] = [
    "collection",
    "name",
    "kind",
    "declared",
    "operational",
    "enabled",
    "build_state",
    "in_sync",
    "queryable",
    "requires_rebuild",
];

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn text<'a>(row: &'a reddb::storage::query::unified::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn bool_field(row: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> bool {
    match row.get(field) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected {field} bool, got {other:?} in {row:?}"),
    }
}

fn cleanup_scope() {
    clear_current_auth_identity();
    clear_current_tenant();
    clear_current_connection_id();
}

fn text_field<'a>(
    record: &'a reddb::storage::query::unified::UnifiedRecord,
    field: &str,
) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text field {field}, got {other:?}"),
    }
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
            "internal",
            "tenant_id"
        ]
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(row.get("name"), Some(&Value::text("users")));
    assert_eq!(row.get("model"), Some(&Value::text("table")));
    assert_eq!(
        row.get("schema_mode"),
        Some(&Value::text("semi_structured"))
    );
    assert_eq!(row.get("entities"), Some(&Value::UnsignedInteger(1)));
    assert!(matches!(
        row.get("indices"),
        Some(Value::UnsignedInteger(_))
    ));
    assert!(matches!(
        row.get("in_memory_bytes"),
        Some(Value::UnsignedInteger(_))
    ));
    assert_eq!(row.get("internal"), Some(&Value::Boolean(false)));

    cleanup_scope();
}

#[test]
fn select_from_red_columns_materializes_table_schema() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT DEFAULT = 'unknown', active BOOLEAN NOT NULL)",
    );

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'users'")
        .expect("red.columns select");

    assert_eq!(
        result.result.columns,
        vec![
            "collection",
            "name",
            "type",
            "nullable",
            "default_value",
            "is_primary_key",
            "is_unique",
        ]
    );
    assert_eq!(result.result.records.len(), 4);

    let id = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "id")
        .expect("id column");
    assert_eq!(text(id, "collection"), "users");
    assert_eq!(text(id, "type"), "INTEGER");
    assert!(!bool_field(id, "nullable"));
    assert!(bool_field(id, "is_primary_key"));
    assert!(bool_field(id, "is_unique"));

    let email = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "email")
        .expect("email column");
    assert!(bool_field(email, "nullable"));
    assert!(bool_field(email, "is_unique"));

    let active = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "active")
        .expect("active column");
    assert_eq!(text(active, "type"), "BOOLEAN");
    assert!(!bool_field(active, "nullable"));

    cleanup_scope();
}

#[test]
fn select_from_red_indices_materializes_index_status_rows() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(
        &rt,
        "CREATE INDEX users_email_idx ON users (email) USING HASH",
    );

    let result = rt
        .execute_query("SELECT * FROM red.indices WHERE collection = 'users'")
        .expect("red.indices select");

    assert_eq!(result.result.columns, INDEX_COLUMNS.map(str::to_string));
    let row = result
        .result
        .records
        .iter()
        .find(|record| text_field(record, "name") == "users_email_idx")
        .expect("users_email_idx row");
    assert_eq!(row.get("collection"), Some(&Value::text("users")));
    assert_eq!(row.get("kind"), Some(&Value::text("hash")));
    assert_eq!(row.get("enabled"), Some(&Value::Boolean(true)));
    assert_eq!(row.get("build_state"), Some(&Value::text("ready")));
    assert_eq!(row.get("queryable"), Some(&Value::Boolean(true)));
    assert_eq!(row.get("requires_rebuild"), Some(&Value::Boolean(false)));

    cleanup_scope();
}

#[test]
fn select_from_red_stats_materializes_collection_counters() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT)");
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'alice')");

    let result = rt
        .execute_query("SELECT * FROM red.stats WHERE collection = 'users'")
        .expect("red.stats select");

    assert_eq!(
        result.result.columns,
        vec![
            "collection",
            "entities",
            "segments",
            "growing_count",
            "sealed_count",
            "archived_count",
            "seal_ops",
            "compact_ops",
            "last_write_ms",
            "attention_score"
        ]
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(row.get("collection"), Some(&Value::text("users")));
    assert_eq!(row.get("entities"), Some(&Value::UnsignedInteger(1)));
    assert!(matches!(
        row.get("segments"),
        Some(Value::UnsignedInteger(_))
    ));
    assert!(matches!(
        row.get("growing_count"),
        Some(Value::UnsignedInteger(_))
    ));
    assert_eq!(row.get("last_write_ms"), Some(&Value::Null));
    assert!(matches!(
        row.get("attention_score"),
        Some(Value::UnsignedInteger(_))
    ));

    cleanup_scope();
}

#[test]
fn show_schema_desugars_to_red_columns_collection_filter() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );

    let via_select = rt
        .execute_query("SELECT name, type FROM red.columns WHERE collection = 'users'")
        .expect("red.columns select");
    let via_show = rt
        .execute_query("SHOW SCHEMA users")
        .expect("SHOW SCHEMA users");

    assert_eq!(
        via_show.result.columns,
        vec![
            "collection",
            "name",
            "type",
            "nullable",
            "default_value",
            "is_primary_key",
            "is_unique"
        ]
    );
    let show_pairs: Vec<_> = via_show
        .result
        .records
        .iter()
        .map(|row| (text(row, "name").to_string(), text(row, "type").to_string()))
        .collect();
    let select_pairs: Vec<_> = via_select
        .result
        .records
        .iter()
        .map(|row| (text(row, "name").to_string(), text(row, "type").to_string()))
        .collect();
    assert_eq!(show_pairs, select_pairs);

    cleanup_scope();
}

#[test]
fn red_columns_infers_document_top_level_fields_as_nullable_schema() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        r#"INSERT INTO logs DOCUMENT (body) VALUES ({"level":"warn","ip":"10.0.0.1"})"#,
    );
    exec(
        &rt,
        r#"INSERT INTO logs DOCUMENT (body) VALUES ({"level":"info","msg":"login"})"#,
    );

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'logs'")
        .expect("document red.columns select");

    let names: Vec<_> = result
        .result
        .records
        .iter()
        .map(|row| text(row, "name").to_string())
        .collect();
    assert!(names.contains(&"body".to_string()), "names = {names:?}");
    assert!(names.contains(&"level".to_string()), "names = {names:?}");
    assert!(names.contains(&"ip".to_string()), "names = {names:?}");
    assert!(names.contains(&"msg".to_string()), "names = {names:?}");

    let level = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "level")
        .expect("level field");
    assert_eq!(text(level, "type"), "TEXT");
    assert!(!bool_field(level, "nullable"));

    let ip = result
        .result
        .records
        .iter()
        .find(|row| text(row, "name") == "ip")
        .expect("ip field");
    assert!(bool_field(ip, "nullable"));

    cleanup_scope();
}

#[test]
fn show_indices_lists_all_and_show_indices_on_filters_collection() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(&rt, "CREATE TABLE orders (id INT, total INT)");
    exec(
        &rt,
        "CREATE INDEX users_email_idx ON users (email) USING HASH",
    );
    exec(
        &rt,
        "CREATE INDEX orders_total_idx ON orders (total) USING BTREE",
    );

    let all = rt.execute_query("SHOW INDICES").expect("SHOW INDICES");
    let all_names: Vec<String> = all
        .result
        .records
        .iter()
        .map(|record| text_field(record, "name").to_string())
        .collect();
    assert!(all_names.iter().any(|name| name == "users_email_idx"));
    assert!(all_names.iter().any(|name| name == "orders_total_idx"));

    let filtered = rt
        .execute_query("SHOW INDICES ON users")
        .expect("SHOW INDICES ON users");
    assert_eq!(filtered.result.columns, INDEX_COLUMNS.map(str::to_string));
    assert!(filtered
        .result
        .records
        .iter()
        .any(|record| text_field(record, "name") == "users_email_idx"));
    assert!(filtered
        .result
        .records
        .iter()
        .all(|record| text_field(record, "collection") == "users"));

    cleanup_scope();
}

#[test]
fn show_stats_desugars_to_red_stats() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "CREATE TABLE projects (id INT)");
    exec(&rt, "INSERT INTO users (id) VALUES (1)");

    let single = rt
        .execute_query("SHOW STATS users")
        .expect("show stats users");
    assert_eq!(single.result.records.len(), 1);
    assert_eq!(
        single.result.records[0].get("collection"),
        Some(&Value::text("users"))
    );
    assert_eq!(
        single.result.records[0].get("attention_score"),
        Some(&Value::UnsignedInteger(0))
    );

    let all = rt.execute_query("SHOW STATS").expect("show stats");
    let collections = all
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("collection") {
            Some(Value::Text(name)) => Some(name.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(collections.contains(&"users"));
    assert!(collections.contains(&"projects"));

    cleanup_scope();
}

#[test]
fn red_columns_returns_empty_for_schemaless_table_contract() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "INSERT INTO scratch (id, note) VALUES (1, 'loose')");

    let result = rt
        .execute_query("SELECT * FROM red.columns WHERE collection = 'scratch'")
        .expect("schemaless red.columns select");

    assert_eq!(result.result.records.len(), 0);
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
