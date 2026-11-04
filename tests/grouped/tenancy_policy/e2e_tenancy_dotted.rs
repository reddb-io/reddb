//! Phase 2 PG parity — tenancy via dotted paths.
//!
//! `CREATE TABLE ... TENANT BY (root.nested)` treats a JSON column's
//! nested key as the tenant discriminator. The RLS policy evaluates
//! the path at read time; INSERT auto-fills the path on every row,
//! either by mutating an existing JSON column or by creating a
//! fresh JSON object when the root column is absent.

use reddb::runtime::mvcc::{
    clear_current_connection_id, clear_current_tenant, set_current_connection_id,
    set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
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
    // `kind` is a reserved system field, so the user column is
    // `event_kind` (mirrors the rename in e2e_within_clause).
    exec(
        &rt,
        "CREATE TABLE events (id INT, event_kind TEXT, meta TEXT) \
         TENANT BY (meta.tenant)",
    );

    // Seed directly with JSON payloads for two tenants.
    exec(
        &rt,
        "INSERT INTO events (id, event_kind, meta) VALUES \
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
    exec(&rt, "INSERT INTO logs (id, msg) VALUES (1, 'started')");

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

#[test]
fn explicit_flat_and_dotted_tenants_must_match_the_active_tenant() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE flat_docs (id INT, tenant_id TEXT, body TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "CREATE TABLE dotted_docs (id INT, headers TEXT) TENANT BY (headers.tenant)",
    );

    set_current_connection_id(501);
    set_current_tenant("acme".to_string());

    assert!(rt
        .execute_query(
            "INSERT INTO flat_docs (id, tenant_id, body) VALUES (1, 'globex', 'blocked')",
        )
        .is_err());
    assert!(rt
        .execute_query(
            "INSERT INTO flat_docs (id, tenant_id, body) VALUES \
             (2, 'acme', 'must-roll-back'), (3, 'globex', 'blocked')",
        )
        .is_err());
    assert!(rt
        .execute_query_with_params(
            "INSERT INTO flat_docs (id, tenant_id, body) VALUES (4, $1, 'blocked')",
            &[Value::text("globex")],
        )
        .is_err());
    assert!(rt
        .execute_query(
            "INSERT INTO flat_docs (id, tenant_id, body) VALUES \
             (5, 'globex', 'blocked') ON CONFLICT DO NOTHING",
        )
        .is_err());
    assert!(rt
        .execute_query(
            "INSERT INTO dotted_docs (id, headers) VALUES \
             (1, '{\"tenant\":\"globex\",\"trace\":\"x\"}')",
        )
        .is_err());
    assert_eq!(row_count(&rt, "SELECT * FROM flat_docs"), 0);
    assert_eq!(row_count(&rt, "SELECT * FROM dotted_docs"), 0);

    exec(
        &rt,
        "INSERT INTO flat_docs (id, tenant_id, body) VALUES (6, 'acme', 'allowed')",
    );
    exec(
        &rt,
        "INSERT INTO dotted_docs (id, headers) VALUES \
         (2, '{\"tenant\":\"acme\",\"trace\":\"ok\"}')",
    );

    clear_current_tenant();
    clear_current_connection_id();
    exec(
        &rt,
        "INSERT INTO flat_docs (id, tenant_id, body) VALUES (7, 'globex', 'platform-loader')",
    );
    set_current_connection_id(502);
    set_current_tenant("acme".to_string());
    assert_eq!(row_count(&rt, "SELECT * FROM flat_docs"), 1);
    assert_eq!(row_count(&rt, "SELECT * FROM dotted_docs"), 1);
    set_current_tenant("globex".to_string());
    assert_eq!(row_count(&rt, "SELECT * FROM flat_docs"), 1);
    assert_eq!(row_count(&rt, "SELECT * FROM dotted_docs"), 0);
    clear_current_connection_id();
}
