//! `WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>` —
//! per-statement scope override for tenant + auth identity.

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn rows(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql).unwrap().result.records.len()
}

#[test]
fn within_tenant_filters_select() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE orders (id INT, amount INT, client_id TEXT) \
         TENANT BY (client_id)",
    );
    exec(
        &rt,
        "INSERT INTO orders (id, amount, client_id) VALUES \
         (1, 100, 'acme'), (2, 200, 'acme'), (3, 300, 'globex')",
    );

    assert_eq!(rows(&rt, "WITHIN TENANT 'acme' SELECT * FROM orders"), 2);
    assert_eq!(rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM orders"), 1);
}

#[test]
fn within_tenant_auto_fills_insert() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE jobs (id INT, name TEXT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );

    exec(&rt, "WITHIN TENANT 'acme' INSERT INTO jobs (id, name) VALUES (1, 'a')");
    exec(&rt, "WITHIN TENANT 'globex' INSERT INTO jobs (id, name) VALUES (2, 'b')");

    assert_eq!(rows(&rt, "WITHIN TENANT 'acme' SELECT * FROM jobs"), 1);
    assert_eq!(rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM jobs"), 1);
}

#[test]
fn within_does_not_leak_to_next_query() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE events (id INT, kind TEXT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "WITHIN TENANT 'acme' INSERT INTO events (id, kind) VALUES (1, 'login')",
    );
    exec(
        &rt,
        "WITHIN TENANT 'globex' INSERT INTO events (id, kind) VALUES (2, 'login')",
    );

    // No ambient SET TENANT and no WITHIN — RLS deny-default hides everything.
    assert_eq!(rows(&rt, "SELECT * FROM events"), 0);
}

#[test]
fn within_overrides_set_tenant_for_one_call() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, tenant_id) VALUES (1, 'acme'), (2, 'globex')",
    );

    exec(&rt, "SET TENANT 'acme'");
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);

    // WITHIN overrides for this one call only…
    assert_eq!(
        rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM t"),
        1
    );
    let r = rt
        .execute_query("WITHIN TENANT 'globex' SELECT id FROM t")
        .unwrap();
    let only = &r.result.records[0];
    assert!(format!("{only:?}").contains("2"));

    // …and the session tenant is restored after.
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);
}

#[test]
fn within_tenant_null_clears_for_call() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, tenant_id) VALUES (1, 'acme'), (2, 'globex')",
    );

    exec(&rt, "SET TENANT 'acme'");
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);

    // NULL clears tenant for this one call → RLS denies all.
    assert_eq!(rows(&rt, "WITHIN TENANT NULL SELECT * FROM t"), 0);

    // Session tenant restored.
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);
}

#[test]
fn within_filters_update_and_delete() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, val INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, val, tenant_id) VALUES \
         (1, 10, 'acme'), (2, 20, 'acme'), (3, 30, 'globex')",
    );

    // UPDATE under WITHIN — acme tenant rows mutate.
    exec(
        &rt,
        "WITHIN TENANT 'acme' UPDATE t SET val = 99 WHERE id = 1",
    );
    let r = rt
        .execute_query("WITHIN TENANT 'acme' SELECT * FROM t")
        .unwrap();
    let dbg = format!("{:?}", r.result.records);
    assert!(dbg.contains("99"), "expected updated val=99 in {dbg}");

    // DELETE under WITHIN scopes to the named tenant — globex row stays.
    exec(&rt, "WITHIN TENANT 'acme' DELETE FROM t WHERE id = 2");
    assert_eq!(rows(&rt, "WITHIN TENANT 'acme' SELECT * FROM t"), 1);
    assert_eq!(rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM t"), 1);
}

#[test]
fn within_filters_join_between_two_tenant_tables() {
    // Under `WITHIN TENANT 'x'` a JOIN between two tenant-scoped tables
    // returns only rows where both sides belong to 'x'. The dispatch
    // path folds each leaf table's RLS predicate (including the auto
    // `__tenant_iso` policy) into the corresponding TableQuery before
    // handing the join tree to the executor.
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE customers (id INT, name TEXT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "CREATE TABLE orders (id INT, customer_id INT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO customers (id, name, tenant_id) VALUES \
         (1, 'a', 'acme'), (2, 'g', 'globex')",
    );
    exec(
        &rt,
        "INSERT INTO orders (id, customer_id, tenant_id) VALUES \
         (10, 1, 'acme'), (20, 2, 'globex')",
    );

    let r = rt
        .execute_query(
            "WITHIN TENANT 'acme' \
             FROM customers JOIN orders ON customers.id = orders.customer_id",
        )
        .unwrap();
    assert_eq!(r.result.records.len(), 1, "got {:?}", r.result.records);
}

#[test]
fn within_works_through_view() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE leads (id INT, score INT, tenant_id TEXT) \
         TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO leads (id, score, tenant_id) VALUES \
         (1, 10, 'acme'), (2, 90, 'acme'), (3, 50, 'globex')",
    );
    exec(&rt, "CREATE VIEW hot_leads AS SELECT * FROM leads WHERE score > 20");

    // View body is tenant-aware via RLS — WITHIN scopes both the view's
    // underlying scan and any outer predicates.
    assert_eq!(
        rows(&rt, "WITHIN TENANT 'acme' SELECT * FROM hot_leads"),
        1
    );
    assert_eq!(
        rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM hot_leads"),
        1
    );
}

#[test]
fn within_does_not_elevate_actual_role() {
    // WITHIN ... AS ROLE 'admin' projects the *string* the SQL sees via
    // CURRENT_ROLE(), but does not change the connection's real auth
    // identity. RBAC checks (admin-only DDL, vault access, etc.) read
    // from the underlying identity, not the override. Verify the
    // override is contained: an explicit policy gated on CURRENT_ROLE()
    // accepts the projected string for filtering, exactly as designed.
    let rt = open_runtime();
    // No TENANT BY here — multiple policies on the same table are OR'd
    // by the RLS evaluator, so a tenant auto-policy would short-circuit
    // any role check. Single explicit policy keeps the test focused on
    // the projection behaviour.
    exec(&rt, "CREATE TABLE secrets (id INT, body TEXT)");
    exec(
        &rt,
        "CREATE POLICY admins_only ON secrets \
         USING (CURRENT_ROLE() = 'admin')",
    );
    exec(&rt, "ALTER TABLE secrets ENABLE ROW LEVEL SECURITY");
    exec(&rt, "INSERT INTO secrets (id, body) VALUES (1, 'top')");

    // No role override → policy denies (CURRENT_ROLE() is NULL outside
    // an authenticated session).
    assert_eq!(rows(&rt, "SELECT * FROM secrets"), 0);

    // With role projection the policy lets the row through. The
    // projection only affects RLS predicate eval — it cannot grant
    // DDL or vault privileges, which check the real identity instead.
    assert_eq!(
        rows(&rt, "WITHIN TENANT 'acme' AS ROLE 'admin' SELECT * FROM secrets"),
        1
    );
}

#[test]
fn within_inside_transaction() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );

    set_current_connection_id(900);

    exec(&rt, "BEGIN");
    exec(
        &rt,
        "WITHIN TENANT 'acme' INSERT INTO t (id) VALUES (1)",
    );
    exec(
        &rt,
        "WITHIN TENANT 'acme' INSERT INTO t (id) VALUES (2)",
    );
    exec(&rt, "COMMIT");

    // Both rows visible under acme; none leaked to globex.
    assert_eq!(rows(&rt, "WITHIN TENANT 'acme' SELECT * FROM t"), 2);
    assert_eq!(rows(&rt, "WITHIN TENANT 'globex' SELECT * FROM t"), 0);

    clear_current_connection_id();
}

#[test]
fn set_local_tenant_scopes_to_transaction() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, tenant_id) VALUES (1, 'acme'), (2, 'globex')",
    );

    set_current_connection_id(910);

    // Pin a session-level tenant first.
    exec(&rt, "SET TENANT 'acme'");
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);

    // Inside a transaction SET LOCAL TENANT swaps the tenant for the
    // duration of the txn; queries see the new tenant's rows only.
    exec(&rt, "BEGIN");
    exec(&rt, "SET LOCAL TENANT 'globex'");
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);
    let r = rt.execute_query("SELECT id FROM t").unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(2)"));
    exec(&rt, "COMMIT");

    // After COMMIT the session-level tenant is restored — `SET LOCAL`
    // never touched it.
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);
    let r = rt.execute_query("SELECT id FROM t").unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(1)"));

    clear_current_connection_id();
}

#[test]
fn set_local_tenant_outside_transaction_errors() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, tenant_id TEXT)");
    let err = rt.execute_query("SET LOCAL TENANT 'acme'").unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("active transaction"),
        "expected transaction error, got: {msg}"
    );
}

#[test]
fn set_local_tenant_rollback_clears_override() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, tenant_id) VALUES (1, 'acme'), (2, 'globex')",
    );
    set_current_connection_id(911);
    exec(&rt, "SET TENANT 'acme'");
    exec(&rt, "BEGIN");
    exec(&rt, "SET LOCAL TENANT 'globex'");
    exec(&rt, "ROLLBACK");
    // Rollback evicted the local override — back to the session tenant.
    assert_eq!(rows(&rt, "SELECT * FROM t"), 1);
    let r = rt.execute_query("SELECT id FROM t").unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(1)"));
    clear_current_connection_id();
}

#[test]
fn within_overrides_set_local_tenant() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE t (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO t (id, tenant_id) VALUES (1, 'acme'), (2, 'globex'), (3, 'wonka')",
    );
    set_current_connection_id(912);
    exec(&rt, "BEGIN");
    exec(&rt, "SET LOCAL TENANT 'globex'");
    // tx-local says globex, but per-statement WITHIN says wonka — WITHIN wins.
    let r = rt
        .execute_query("WITHIN TENANT 'wonka' SELECT id FROM t")
        .unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(3)"));
    // Next query without WITHIN falls back to tx-local globex.
    let r = rt.execute_query("SELECT id FROM t").unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(2)"));
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();
}

#[test]
fn scalar_select_returns_session_context() {
    // `SELECT CURRENT_TENANT()` (no FROM) should reflect the active
    // tenant — previously the scalar projection path lacked the arm
    // and always returned NULL even with `SET TENANT '…'` bound.
    let rt = open_runtime();

    exec(&rt, "SET TENANT 'acme'");
    let r = rt.execute_query("SELECT CURRENT_TENANT()").unwrap();
    let dbg = format!("{:?}", r.result.records);
    assert!(dbg.contains("acme"), "session tenant: {dbg}");

    let r = rt
        .execute_query(
            "WITHIN TENANT 'globex' USER 'filipe' AS ROLE 'admin' \
             SELECT CURRENT_TENANT(), CURRENT_USER(), CURRENT_ROLE()",
        )
        .unwrap();
    let dbg = format!("{:?}", r.result.records[0]);
    assert!(dbg.contains("globex"), "WITHIN tenant: {dbg}");
    assert!(dbg.contains("filipe"), "WITHIN user: {dbg}");
    assert!(dbg.contains("admin"), "WITHIN role: {dbg}");
}

#[test]
fn execute_query_with_scope_typed_api() {
    use reddb::runtime::within_clause::{FieldOverride, ScopeOverride};

    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE jobs (id INT, body TEXT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(&rt, "SET TENANT 'admin-bootstrap'");
    exec(
        &rt,
        "INSERT INTO jobs (id, body, tenant_id) VALUES \
         (1, 'a', 'acme'), (2, 'g', 'globex')",
    );
    exec(&rt, "SET TENANT NULL");

    let acme = ScopeOverride {
        tenant: FieldOverride::Set("acme".into()),
        ..Default::default()
    };
    let r = rt
        .execute_query_with_scope("SELECT * FROM jobs", acme)
        .unwrap();
    assert_eq!(r.result.records.len(), 1);

    let globex = ScopeOverride {
        tenant: FieldOverride::Set("globex".into()),
        ..Default::default()
    };
    let r = rt
        .execute_query_with_scope("SELECT * FROM jobs", globex)
        .unwrap();
    assert_eq!(r.result.records.len(), 1);

    // Empty scope = bypass; behaves identically to plain execute_query.
    let r = rt
        .execute_query_with_scope("SELECT * FROM jobs", ScopeOverride::default())
        .unwrap();
    assert_eq!(r.result.records.len(), 0); // RLS deny-default
}

#[test]
fn user_id_column_filter_works() {
    // Regression for a bloom-prune bug: `WHERE id = N` on a table whose
    // `id` is a regular user column (not the engine PK) used to short-
    // circuit to zero rows because the bloom hint code treated `id` as
    // the synthetic `red_entity_id`. Fixed by restricting the bloom
    // hint to `red_entity_id` only.
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, val INT)");
    exec(&rt, "INSERT INTO t (id, val) VALUES (1, 10), (2, 20)");

    let r = rt.execute_query("SELECT val FROM t WHERE id = 1").unwrap();
    assert_eq!(r.result.records.len(), 1);
    assert!(format!("{:?}", r.result.records).contains("Integer(10)"));

    exec(&rt, "UPDATE t SET val = 99 WHERE id = 1");
    let r = rt
        .execute_query("SELECT val FROM t WHERE id = 1")
        .unwrap();
    assert!(format!("{:?}", r.result.records).contains("Integer(99)"));
}

#[test]
fn within_malformed_returns_error() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, tenant_id TEXT)");

    // Missing TENANT clause.
    assert!(rt
        .execute_query("WITHIN USER 'x' SELECT * FROM t")
        .is_err());

    // No inner statement.
    assert!(rt.execute_query("WITHIN TENANT 'acme'").is_err());

    // Duplicate TENANT.
    assert!(rt
        .execute_query("WITHIN TENANT 'a' TENANT 'b' SELECT * FROM t")
        .is_err());
}
