//! Phase 5 PG parity — views over arbitrary query bodies.
//!
//! RedDB views store a full `QueryExpr` as the body, not just a
//! SELECT statement — a view can wrap a filtered SELECT, a JOIN, a
//! MATCH / graph walk, a vector search, or an ASK. The rewriter
//! substitutes the body whenever the view name is referenced.
//!
//! This test focuses on the shippable core:
//! * `CREATE VIEW x AS SELECT ... WHERE ...` — body with filter
//! * `SELECT * FROM x` — rewrites to the body, executes the filter
//! * `CREATE OR REPLACE VIEW x AS ...` — re-point the view
//! * `CREATE MATERIALIZED VIEW` + `REFRESH MATERIALIZED VIEW`

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

#[test]
fn view_body_filters_rows_via_select_from_view() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INT, email TEXT, active BOOLEAN)",
    );
    exec(
        &rt,
        "INSERT INTO users (id, email, active) VALUES \
           (1, 'a@x', true), \
           (2, 'b@x', false), \
           (3, 'c@x', true)",
    );

    // View wrapping a filtered SELECT.
    exec(
        &rt,
        "CREATE VIEW active_users AS SELECT id, email FROM users WHERE active = true",
    );

    // The view hides the inactive row.
    let result = exec(&rt, "SELECT * FROM active_users");
    assert_eq!(result.result.records.len(), 2);

    // Re-point the view with OR REPLACE.
    exec(
        &rt,
        "CREATE OR REPLACE VIEW active_users AS \
         SELECT id, email FROM users WHERE active = false",
    );
    let result = exec(&rt, "SELECT * FROM active_users");
    assert_eq!(result.result.records.len(), 1);

    // DROP VIEW cleans up.
    exec(&rt, "DROP VIEW active_users");
    assert!(
        rt.execute_query("SELECT * FROM active_users").is_err()
            || rt
                .execute_query("SELECT * FROM active_users")
                .unwrap()
                .result
                .records
                .is_empty()
    );
}

#[test]
fn materialized_view_refresh_executes_body() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE orders (id INT, total INT, status TEXT)");
    exec(
        &rt,
        "INSERT INTO orders (id, total, status) VALUES \
           (1, 100, 'paid'), \
           (2, 200, 'paid'), \
           (3, 300, 'pending')",
    );

    exec(
        &rt,
        "CREATE MATERIALIZED VIEW paid_orders AS \
         SELECT * FROM orders WHERE status = 'paid'",
    );

    // REFRESH runs the body — should succeed without error.
    let result = exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
    assert_eq!(result.statement_type, "refresh_materialized_view");

    // Selecting the materialized view name also executes the body
    // (the rewriter descends into it just like a regular view).
    let result = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(result.result.records.len(), 2);
}

#[test]
fn view_chain_resolves_recursively() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE events (id INT, severity TEXT, module TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO events (id, severity, module) VALUES \
           (1, 'info',  'auth'), \
           (2, 'warn',  'db'),   \
           (3, 'error', 'auth'), \
           (4, 'error', 'db')",
    );

    // Base view: errors only.
    exec(
        &rt,
        "CREATE VIEW error_events AS SELECT * FROM events WHERE severity = 'error'",
    );
    // Stacked view referencing the first one.
    exec(
        &rt,
        "CREATE VIEW auth_errors AS SELECT * FROM error_events WHERE module = 'auth'",
    );

    let result = exec(&rt, "SELECT * FROM auth_errors");
    assert_eq!(
        result.result.records.len(),
        1,
        "stacked view should filter recursively (only id=3)"
    );
}
