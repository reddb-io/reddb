//! Savepoints end-to-end tests (T5 / PG gap item #8a).
//!
//! Exercises SAVEPOINT / RELEASE / ROLLBACK TO across one embedded
//! connection. Savepoint state lives on `tx_contexts[conn_id]`, so
//! each test pins a deterministic connection id for the duration.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn try_exec(rt: &RedDBRuntime, sql: &str) -> Result<(), String> {
    rt.execute_query(sql).map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn savepoint_release_keeps_inner_writes() {
    let rt = rt();
    set_current_connection_id(1001);

    exec(&rt, "CREATE TABLE sp_release (id INT, label TEXT)");
    exec(&rt, "BEGIN");
    exec(&rt, "INSERT INTO sp_release (id, label) VALUES (1, 'outer')");
    exec(&rt, "SAVEPOINT sp1");
    exec(&rt, "INSERT INTO sp_release (id, label) VALUES (2, 'inner')");
    exec(&rt, "RELEASE SAVEPOINT sp1");
    exec(&rt, "COMMIT");

    let result = rt
        .execute_query("SELECT id FROM sp_release")
        .expect("select after commit");
    assert_eq!(
        result.result.records.len(),
        2,
        "RELEASE keeps both rows committed"
    );

    clear_current_connection_id();
}

#[test]
fn rollback_to_savepoint_discards_inner_writes() {
    let rt = rt();
    set_current_connection_id(1002);

    exec(&rt, "CREATE TABLE sp_rollback (id INT, label TEXT)");
    exec(&rt, "BEGIN");
    exec(&rt, "INSERT INTO sp_rollback (id, label) VALUES (1, 'keep')");
    exec(&rt, "SAVEPOINT sp1");
    exec(&rt, "INSERT INTO sp_rollback (id, label) VALUES (2, 'drop')");
    exec(&rt, "ROLLBACK TO SAVEPOINT sp1");
    exec(&rt, "COMMIT");

    let result = rt
        .execute_query("SELECT id FROM sp_rollback")
        .expect("select after commit");
    assert_eq!(
        result.result.records.len(),
        1,
        "ROLLBACK TO must discard the inner insert"
    );

    clear_current_connection_id();
}

#[test]
fn nested_savepoints_release_pops_inner() {
    let rt = rt();
    set_current_connection_id(1003);

    exec(&rt, "CREATE TABLE sp_nested (id INT)");
    exec(&rt, "BEGIN");
    exec(&rt, "SAVEPOINT a");
    exec(&rt, "INSERT INTO sp_nested (id) VALUES (1)");
    exec(&rt, "SAVEPOINT b");
    exec(&rt, "INSERT INTO sp_nested (id) VALUES (2)");
    // RELEASE a also pops b (PG semantics — nested released together).
    exec(&rt, "RELEASE SAVEPOINT a");
    // b must no longer be addressable.
    let err =
        try_exec(&rt, "ROLLBACK TO SAVEPOINT b").expect_err("b should be gone after release a");
    assert!(
        err.contains("savepoint b"),
        "error should mention savepoint b, got: {err}"
    );
    exec(&rt, "COMMIT");

    let result = rt.execute_query("SELECT id FROM sp_nested").unwrap();
    assert_eq!(
        result.result.records.len(),
        2,
        "both inserts stay after RELEASE of outer"
    );

    clear_current_connection_id();
}

#[test]
fn rollback_to_outer_savepoint_drops_inner_nested() {
    let rt = rt();
    set_current_connection_id(1004);

    exec(&rt, "CREATE TABLE sp_cascade (id INT)");
    exec(&rt, "BEGIN");
    exec(&rt, "INSERT INTO sp_cascade (id) VALUES (1)");
    exec(&rt, "SAVEPOINT outer_sp");
    exec(&rt, "INSERT INTO sp_cascade (id) VALUES (2)");
    exec(&rt, "SAVEPOINT inner_sp");
    exec(&rt, "INSERT INTO sp_cascade (id) VALUES (3)");
    exec(&rt, "ROLLBACK TO SAVEPOINT outer_sp");
    exec(&rt, "COMMIT");

    let result = rt.execute_query("SELECT id FROM sp_cascade").unwrap();
    assert_eq!(
        result.result.records.len(),
        1,
        "rollback to outer must cascade through inner_sp"
    );

    clear_current_connection_id();
}

#[test]
fn savepoint_outside_transaction_is_noop() {
    let rt = rt();
    set_current_connection_id(1005);
    // Should not error; runtime treats it as a no-op.
    exec(&rt, "SAVEPOINT orphan");
    exec(&rt, "RELEASE SAVEPOINT orphan");
    clear_current_connection_id();
}

#[test]
fn rollback_to_unknown_savepoint_errors() {
    let rt = rt();
    set_current_connection_id(1006);
    exec(&rt, "BEGIN");
    let err = try_exec(&rt, "ROLLBACK TO SAVEPOINT does_not_exist")
        .expect_err("unknown savepoint must error");
    assert!(
        err.contains("does_not_exist"),
        "error should name the missing savepoint, got: {err}"
    );
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();
}
