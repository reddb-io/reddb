//! Phase 2 PG parity — tenancy via dotted paths.
//!
//! `CREATE TABLE ... TENANT BY (root.nested)` treats a JSON column's
//! nested key as the tenant discriminator. The RLS policy evaluates
//! the path at read time; INSERT auto-fills the path on every row,
//! either by mutating an existing JSON column or by creating a
//! fresh JSON object when the root column is absent.

use reddb::runtime::mvcc::{
    clear_current_connection_id, set_current_connection_id, set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    let result = rt.execute_query(sql).unwrap();
    result.result.records.len()
}

#[test]
fn dotted_tenant_path_filters_reads() {
    let rt = open_runtime();

    // JSON metadata column, tenant lives under `metadata.tenant`.
    exec(
        &rt,
        "CREATE TABLE events (id INT, kind TEXT, meta TEXT) \
         TENANT BY (meta.tenant)",
    );

    // Seed directly with JSON payloads for two tenants.
    exec(
        &rt,
        "INSERT INTO events (id, kind, meta) VALUES \
           (1, 'login', '{\"tenant\": \"acme\", \"ip\": \"1.2.3.4\"}'), \
           (2, 'login', '{\"tenant\": \"globex\", \"ip\": \"5.6.7.8\"}'), \
           (3, 'signup', '{\"tenant\": \"acme\"}')",
    );

    set_current_connection_id(201);

    // Acme: two matching rows.
    set_current_tenant("acme".to_string());
    assert_eq!(
        row_count(&rt, "SELECT * FROM events"),
        2,
        "acme should see 2 rows via metadata.tenant path"
    );

    // Globex: one.
    set_current_tenant("globex".to_string());
    assert_eq!(
        row_count(&rt, "SELECT * FROM events"),
        1,
        "globex should see 1 row via metadata.tenant path"
    );

    clear_current_connection_id();
}

#[test]
fn dotted_tenant_auto_fills_missing_root() {
    let rt = open_runtime();

    exec(
        &rt,
        "CREATE TABLE logs (id INT, msg TEXT, headers TEXT) \
         TENANT BY (headers.tenant)",
    );

    set_current_connection_id(301);
    set_current_tenant("acme".to_string());

    // INSERT without mentioning `headers` — auto-fill builds
    // `{"tenant": "acme"}` and appends it as the headers column.
    exec(
        &rt,
        "INSERT INTO logs (id, msg) VALUES (1, 'started')",
    );

    // Same session reads it back: visible because tenant matches.
    assert_eq!(row_count(&rt, "SELECT * FROM logs"), 1);

    // Switch tenant: row is hidden by the auto-policy.
    set_current_tenant("globex".to_string());
    assert_eq!(row_count(&rt, "SELECT * FROM logs"), 0);

    clear_current_connection_id();
}

#[test]
fn dotted_tenant_merges_existing_root_json() {
    let rt = open_runtime();

    exec(
        &rt,
        "CREATE TABLE audit (id INT, headers TEXT) TENANT BY (headers.tenant)",
    );

    set_current_connection_id(401);
    set_current_tenant("acme".to_string());

    // User supplies `headers` JSON but omits the `tenant` nested
    // key — auto-fill must merge without losing existing keys.
    exec(
        &rt,
        "INSERT INTO audit (id, headers) VALUES (1, '{\"trace_id\": \"abc123\"}')",
    );

    // Acme can see it.
    assert_eq!(row_count(&rt, "SELECT * FROM audit"), 1);

    // Globex cannot.
    set_current_tenant("globex".to_string());
    assert_eq!(row_count(&rt, "SELECT * FROM audit"), 0);

    clear_current_connection_id();
}
