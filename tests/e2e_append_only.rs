//! End-to-end tests for `CREATE TABLE ... APPEND ONLY`.
//!
//! The feature is a first-class catalog flag: the runtime rejects
//! UPDATE / DELETE before RLS, before RETURNING, before any scan is
//! even planned. Error messages name the table and the DDL so the
//! operator can self-service the fix.
//!
//! Non-goal of this sprint: `ALTER TABLE ... SET/UNSET APPEND_ONLY`.
//! When that lands, add a test here that flips the flag at runtime
//! and verifies the DML surface reacts correctly — the contract
//! mutator is the only moving piece.

use reddb::application::ExecuteQueryInput;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(q: &QueryUseCases<'_, RedDBRuntime>, sql: &str) {
    q.execute(ExecuteQueryInput { query: sql.into() })
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

fn exec_err(q: &QueryUseCases<'_, RedDBRuntime>, sql: &str) -> String {
    match q.execute(ExecuteQueryInput { query: sql.into() }) {
        Ok(_) => panic!("expected error for: {sql}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn append_only_table_accepts_inserts() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE audit_log (id INT, msg TEXT) APPEND ONLY");
    exec(&q, "INSERT INTO audit_log (id, msg) VALUES (1, 'hello')");
    exec(&q, "INSERT INTO audit_log (id, msg) VALUES (2, 'world')");
    let result = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM audit_log".into(),
        })
        .expect("select should succeed");
    assert_eq!(result.result.records.len(), 2);
}

#[test]
fn append_only_table_rejects_update_with_clear_message() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE events (id INT, v TEXT) APPEND ONLY");
    exec(&q, "INSERT INTO events (id, v) VALUES (1, 'x')");
    let err = exec_err(&q, "UPDATE events SET v = 'y' WHERE id = 1");
    assert!(err.contains("events"), "error names the table: {err}");
    assert!(err.contains("APPEND ONLY"), "error cites DDL: {err}");
    assert!(err.contains("UPDATE"), "error names the operation: {err}");
    // Data must be unchanged.
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT v FROM events WHERE id = 1".into(),
        })
        .unwrap();
    let v = sel.result.records[0]
        .values
        .get("v")
        .expect("v present")
        .to_string();
    assert!(v.contains('x'), "v must stay 'x': {v}");
}

#[test]
fn append_only_table_rejects_delete_with_clear_message() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE ledger (id INT, amt INT) APPEND ONLY");
    exec(&q, "INSERT INTO ledger (id, amt) VALUES (1, 100)");
    let err = exec_err(&q, "DELETE FROM ledger WHERE id = 1");
    assert!(err.contains("ledger"));
    assert!(err.contains("APPEND ONLY"));
    assert!(err.contains("DELETE"));
    // Row still there.
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM ledger".into(),
        })
        .unwrap();
    assert_eq!(sel.result.records.len(), 1);
}

#[test]
fn with_append_only_true_is_equivalent_to_trailing_keyword() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(
        &q,
        "CREATE TABLE metrics (id INT, val INT) WITH (append_only = true)",
    );
    exec(&q, "INSERT INTO metrics (id, val) VALUES (1, 10)");
    let err = exec_err(&q, "UPDATE metrics SET val = 20 WHERE id = 1");
    assert!(err.contains("APPEND ONLY"));
}

#[test]
fn non_append_only_table_keeps_mutable_semantics() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE users (id INT, name TEXT)");
    exec(&q, "INSERT INTO users (id, name) VALUES (1, 'alice')");
    // UPDATE must succeed — default is mutable.
    exec(&q, "UPDATE users SET name = 'bob' WHERE id = 1");
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT name FROM users WHERE id = 1".into(),
        })
        .unwrap();
    let name = sel.result.records[0]
        .values
        .get("name")
        .unwrap()
        .to_string();
    assert!(name.contains("bob"));
}

#[test]
fn append_only_still_allows_select_and_insert_returning() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE trace (id INT, span TEXT) APPEND ONLY");
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO trace (id, span) VALUES (1, 'root') RETURNING span".into(),
        })
        .expect("INSERT RETURNING on APPEND ONLY must succeed");
    assert_eq!(result.result.records.len(), 1);
}
