//! Issue #745 — typed `red.*` relations for tables / documents / KV.
//!
//! Pins the contract that the Red UI toolbars depend on:
//!
//! 1. `red.tables` exposes table-shaped columns
//!    (`name, schema_mode, row_count, column_count, index_count,
//!    has_primary_key, in_memory_bytes, on_disk_bytes, tenant_id,
//!    internal`) — no document/KV/queue noise.
//! 2. `red.documents` exposes document-shaped columns
//!    (`name, schema_mode, document_count, inferred_field_count,
//!    supports_json_path, in_memory_bytes, on_disk_bytes, internal`).
//!    `supports_json_path` is a stable capability indicator (true).
//! 3. `red.kv` exposes KV-shaped columns
//!    (`name, entries, key_type, value_type, supports_prefix_scan,
//!    in_memory_bytes, on_disk_bytes, internal`).
//!    `supports_prefix_scan` is a stable capability indicator (true).
//! 4. Each relation only returns rows for its model — `red.tables`
//!    skips documents and KV stores, etc.
//! 5. Tenant scope is respected like other `red.*` surfaces: rows
//!    created under a tenant are hidden from a different tenant's
//!    session but visible to cluster admin.

use std::collections::HashSet;

use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBOptions;

const TABLE_COLUMNS: [&str; 10] = [
    "name",
    "schema_mode",
    "row_count",
    "column_count",
    "index_count",
    "has_primary_key",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
];

const DOCUMENT_COLUMNS: [&str; 8] = [
    "name",
    "schema_mode",
    "document_count",
    "inferred_field_count",
    "supports_json_path",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

const KV_COLUMNS: [&str; 8] = [
    "name",
    "entries",
    "key_type",
    "value_type",
    "supports_prefix_scan",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn select(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn find_row<'a>(records: &'a [UnifiedRecord], name: &str) -> Option<&'a UnifiedRecord> {
    records.iter().find(|record| match record.get("name") {
        Some(Value::Text(value)) => value.as_ref() == name,
        _ => false,
    })
}

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column `{column}`, got {other:?}"),
    }
}

fn uint(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned column `{column}`, got {other:?}"),
    }
}

fn boolean(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column `{column}`, got {other:?}"),
    }
}

fn names(records: &[UnifiedRecord]) -> HashSet<String> {
    records
        .iter()
        .filter_map(|record| match record.get("name") {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        })
        .collect()
}

fn assert_columns(actual: &[String], expected: &[&str]) {
    let actual: HashSet<&str> = actual.iter().map(String::as_str).collect();
    let expected: HashSet<&str> = expected.iter().copied().collect();
    assert_eq!(
        actual, expected,
        "column set must match the typed-relation contract"
    );
}

#[test]
fn red_tables_exposes_table_shaped_columns() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE orders (id INT PRIMARY KEY, customer TEXT)",
    );
    exec(&rt, "CREATE INDEX orders_customer ON orders (customer)");
    exec(&rt, "INSERT INTO orders (id, customer) VALUES (1, 'alice')");
    exec(&rt, "INSERT INTO orders (id, customer) VALUES (2, 'bob')");

    let result = select(&rt, "SELECT * FROM red.tables");
    assert_columns(&result.result.columns, &TABLE_COLUMNS);

    let orders = find_row(&result.result.records, "orders").expect("orders row");
    assert_eq!(uint(orders, "row_count"), 2);
    assert!(
        uint(orders, "column_count") >= 2,
        "column_count must include at least the two declared columns"
    );
    assert!(
        uint(orders, "index_count") >= 1,
        "index_count must reflect the declared orders_customer index"
    );
    assert!(
        boolean(orders, "has_primary_key"),
        "orders has PRIMARY KEY id"
    );
    assert!(!boolean(orders, "internal"));
}

#[test]
fn red_tables_skips_documents_and_kv() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE plain_table (id INT)");
    exec(&rt, "CREATE DOCUMENT plain_docs");
    exec(&rt, "CREATE KV plain_kv");

    let result = select(&rt, "SELECT name FROM red.tables");
    let names = names(&result.result.records);
    assert!(
        names.contains("plain_table"),
        "expected plain_table in {names:?}"
    );
    assert!(
        !names.contains("plain_docs"),
        "red.tables must not include documents: {names:?}"
    );
    assert!(
        !names.contains("plain_kv"),
        "red.tables must not include KV stores: {names:?}"
    );
}

#[test]
fn red_documents_exposes_document_shaped_columns() {
    let rt = open_runtime();
    exec(&rt, "CREATE DOCUMENT events");
    exec(
        &rt,
        r#"INSERT INTO events DOCUMENT (body) VALUES
        ('{"event":"click","path":"/"}'),
        ('{"event":"view","path":"/about"}')"#,
    );

    let result = select(&rt, "SELECT * FROM red.documents");
    assert_columns(&result.result.columns, &DOCUMENT_COLUMNS);

    let events = find_row(&result.result.records, "events").expect("events row");
    assert_eq!(uint(events, "document_count"), 2);
    assert!(
        uint(events, "inferred_field_count") >= 1,
        "should infer at least the body field"
    );
    assert!(
        boolean(events, "supports_json_path"),
        "document collections always support JSON path"
    );
    assert!(!boolean(events, "internal"));
}

#[test]
fn red_documents_skips_tables_and_kv() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE table_only (id INT)");
    exec(&rt, "CREATE DOCUMENT doc_only");
    exec(&rt, "CREATE KV kv_only");

    let result = select(&rt, "SELECT name FROM red.documents");
    let names = names(&result.result.records);
    assert!(names.contains("doc_only"), "doc_only missing: {names:?}");
    assert!(
        !names.contains("table_only"),
        "red.documents must not include tables: {names:?}"
    );
    assert!(
        !names.contains("kv_only"),
        "red.documents must not include KV stores: {names:?}"
    );
}

#[test]
fn red_kv_exposes_kv_shaped_columns() {
    let rt = open_runtime();
    exec(&rt, "CREATE KV settings");
    exec(&rt, "KV PUT settings.'tenant:mode' = 'dark'");
    exec(&rt, "KV PUT settings.'tenant:theme' = 'midnight'");

    let result = select(&rt, "SELECT * FROM red.kv");
    assert_columns(&result.result.columns, &KV_COLUMNS);

    let settings = find_row(&result.result.records, "settings").expect("settings row");
    assert!(
        uint(settings, "entries") >= 2,
        "expected at least the two PUT'd keys"
    );
    assert!(
        boolean(settings, "supports_prefix_scan"),
        "KV always supports prefix scan"
    );
    // key/value shape is reported as stable text fields.
    let _ = text(settings, "key_type");
    let _ = text(settings, "value_type");
    assert!(!boolean(settings, "internal"));
}

#[test]
fn red_kv_skips_tables_and_documents() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE only_table (id INT)");
    exec(&rt, "CREATE DOCUMENT only_docs");
    exec(&rt, "CREATE KV only_kv");

    let result = select(&rt, "SELECT name FROM red.kv");
    let names = names(&result.result.records);
    assert!(names.contains("only_kv"), "only_kv missing: {names:?}");
    assert!(
        !names.contains("only_table"),
        "red.kv must not include tables: {names:?}"
    );
    assert!(
        !names.contains("only_docs"),
        "red.kv must not include documents: {names:?}"
    );
}

#[test]
fn red_typed_relations_respect_tenant_scope() {
    let rt = open_runtime();
    exec(&rt, "SET TENANT 'acme'");
    exec(
        &rt,
        "CREATE TABLE acme_orders (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(&rt, "CREATE DOCUMENT acme_events");
    exec(&rt, "CREATE KV acme_kv");

    exec(&rt, "SET TENANT 'globex'");
    exec(
        &rt,
        "CREATE TABLE globex_orders (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );

    // Globex session — only globex-owned rows visible.
    let tables = names(&select(&rt, "SELECT name FROM red.tables").result.records);
    assert!(
        tables.contains("globex_orders"),
        "globex sees its own table: {tables:?}"
    );
    assert!(
        !tables.contains("acme_orders"),
        "globex must not see acme's tables: {tables:?}"
    );

    let docs = names(&select(&rt, "SELECT name FROM red.documents").result.records);
    assert!(
        !docs.contains("acme_events"),
        "globex must not see acme's documents: {docs:?}"
    );

    let kv = names(&select(&rt, "SELECT name FROM red.kv").result.records);
    assert!(
        !kv.contains("acme_kv"),
        "globex must not see acme's KV: {kv:?}"
    );

    // Cluster admin (no tenant) sees everything.
    exec(&rt, "SET TENANT NULL");
    let admin_tables = names(&select(&rt, "SELECT name FROM red.tables").result.records);
    assert!(admin_tables.contains("acme_orders"));
    assert!(admin_tables.contains("globex_orders"));
    let admin_docs = names(&select(&rt, "SELECT name FROM red.documents").result.records);
    assert!(admin_docs.contains("acme_events"));
    let admin_kv = names(&select(&rt, "SELECT name FROM red.kv").result.records);
    assert!(admin_kv.contains("acme_kv"));
}
