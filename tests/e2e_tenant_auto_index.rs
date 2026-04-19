//! Auto-index for `TENANT BY` tables.
//!
//! Declaring `TENANT BY (col)` (or retrofitting via `ALTER TABLE ...
//! ENABLE TENANCY ON (col)`) should create a hash index on the tenant
//! column automatically, since every read/write picks up an implicit
//! `col = CURRENT_TENANT()` predicate from the auto-policy.

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn auto_idx_name(table: &str) -> String {
    format!("__tenant_idx_{table}")
}

fn has_index(rt: &RedDBRuntime, table: &str, name: &str) -> bool {
    rt.index_store_ref()
        .list_indices(table)
        .into_iter()
        .any(|i| i.name == name)
}

#[test]
fn create_table_with_tenant_by_creates_auto_index() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE orders (id INT, amount DECIMAL, client_id TEXT) \
         TENANT BY (client_id)",
    );
    assert!(
        has_index(&rt, "orders", &auto_idx_name("orders")),
        "auto tenant index missing after CREATE TABLE"
    );
    let idx = rt
        .index_store_ref()
        .find_index_for_column("orders", "client_id")
        .expect("registry should contain the auto index");
    assert_eq!(idx.columns, vec!["client_id".to_string()]);
}

#[test]
fn create_table_without_tenant_by_does_not_create_index() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE plain (id INT, name TEXT)");
    assert!(rt.index_store_ref().list_indices("plain").is_empty());
}

#[test]
fn alter_enable_tenancy_creates_index_over_existing_data() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE invoices (id INT, total DECIMAL, org TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO invoices (id, total, org) VALUES (1, 100, 'acme'), (2, 200, 'globex')",
    );
    exec(&rt, "ALTER TABLE invoices ENABLE TENANCY ON (org)");
    assert!(has_index(&rt, "invoices", &auto_idx_name("invoices")));
}

#[test]
fn alter_disable_tenancy_drops_auto_index() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE docs (id INT, body TEXT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );
    assert!(has_index(&rt, "docs", &auto_idx_name("docs")));
    exec(&rt, "ALTER TABLE docs DISABLE TENANCY");
    assert!(!has_index(&rt, "docs", &auto_idx_name("docs")));
}

#[test]
fn dotted_tenant_path_skips_auto_index() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE events (id INT, kind TEXT, meta TEXT) \
         TENANT BY (meta.tenant)",
    );
    // Dotted paths aren't covered by flat secondary indices today.
    assert!(rt.index_store_ref().list_indices("events").is_empty());
}

#[test]
fn user_index_on_tenant_column_prevents_duplicate() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE leads (id INT, owner TEXT)");
    exec(&rt, "CREATE INDEX idx_leads_owner ON leads (owner)");
    exec(&rt, "ALTER TABLE leads ENABLE TENANCY ON (owner)");
    let names: Vec<String> = rt
        .index_store_ref()
        .list_indices("leads")
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert!(
        !names.contains(&auto_idx_name("leads")),
        "auto index should be skipped when user index already covers column, got {:?}",
        names
    );
    assert!(names.contains(&"idx_leads_owner".to_string()));
}
