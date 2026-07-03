//! Serializable Snapshot Isolation rw-antidependency litmus tests.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

fn int_cell(rt: &RedDBRuntime, sql: &str, column: &str) -> i64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    match result.result.records[0].get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer column {column}, got {other:?}"),
    }
}

fn assert_serialization_conflict(message: &str) {
    assert!(
        message.contains("serialization conflict"),
        "expected serialization conflict, got {message}"
    );
}

#[test]
fn snapshot_write_skew_allows_both_commits() {
    let rt = rt();
    set_current_connection_id(164_701);
    exec(
        &rt,
        "CREATE TABLE ssi_snapshot_accounts (id INT, balance INT)",
    );
    exec(
        &rt,
        "INSERT INTO ssi_snapshot_accounts (id, balance) VALUES (1, 100), (2, 100)",
    );

    set_current_connection_id(164_702);
    exec(&rt, "BEGIN ISOLATION LEVEL SNAPSHOT");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_snapshot_accounts",
            "total"
        ),
        200
    );

    set_current_connection_id(164_703);
    exec(&rt, "BEGIN ISOLATION LEVEL SNAPSHOT");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_snapshot_accounts",
            "total"
        ),
        200
    );

    set_current_connection_id(164_702);
    exec(
        &rt,
        "UPDATE ssi_snapshot_accounts SET balance = -100 WHERE id = 1",
    );
    set_current_connection_id(164_703);
    exec(
        &rt,
        "UPDATE ssi_snapshot_accounts SET balance = -100 WHERE id = 2",
    );

    set_current_connection_id(164_702);
    exec(&rt, "COMMIT");
    set_current_connection_id(164_703);
    exec(&rt, "COMMIT");

    set_current_connection_id(164_704);
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_snapshot_accounts",
            "total"
        ),
        -200
    );
    clear_current_connection_id();
}

#[test]
fn serializable_write_skew_aborts_one_commit() {
    let rt = rt();
    set_current_connection_id(164_711);
    exec(
        &rt,
        "CREATE TABLE ssi_serializable_accounts (id INT, balance INT)",
    );
    exec(
        &rt,
        "INSERT INTO ssi_serializable_accounts (id, balance) VALUES (1, 100), (2, 100)",
    );

    set_current_connection_id(164_712);
    exec(&rt, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_serializable_accounts",
            "total"
        ),
        200
    );

    set_current_connection_id(164_713);
    exec(&rt, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_serializable_accounts",
            "total"
        ),
        200
    );

    set_current_connection_id(164_712);
    exec(
        &rt,
        "UPDATE ssi_serializable_accounts SET balance = -100 WHERE id = 1",
    );
    set_current_connection_id(164_713);
    exec(
        &rt,
        "UPDATE ssi_serializable_accounts SET balance = -100 WHERE id = 2",
    );

    set_current_connection_id(164_712);
    exec(&rt, "COMMIT");
    set_current_connection_id(164_713);
    assert_serialization_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(164_714);
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(balance) AS total FROM ssi_serializable_accounts",
            "total"
        ),
        0
    );
    clear_current_connection_id();
}

#[test]
fn serializable_three_transaction_dangerous_structure_aborts_pivot() {
    let rt = rt();
    set_current_connection_id(164_721);
    exec(&rt, "CREATE TABLE ssi_dangerous_chain (id INT, value INT)");
    exec(
        &rt,
        "INSERT INTO ssi_dangerous_chain (id, value) VALUES (1, 10), (2, 20)",
    );

    set_current_connection_id(164_722);
    exec(&rt, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(value) AS total FROM ssi_dangerous_chain WHERE id = 1",
            "total"
        ),
        10
    );

    set_current_connection_id(164_723);
    exec(&rt, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(value) AS total FROM ssi_dangerous_chain WHERE id = 2",
            "total"
        ),
        20
    );

    set_current_connection_id(164_724);
    exec(&rt, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    exec(
        &rt,
        "UPDATE ssi_dangerous_chain SET value = 21 WHERE id = 2",
    );
    exec(&rt, "COMMIT");

    set_current_connection_id(164_723);
    exec(
        &rt,
        "UPDATE ssi_dangerous_chain SET value = 11 WHERE id = 1",
    );
    assert_serialization_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(164_722);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(164_725);
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(value) AS total FROM ssi_dangerous_chain WHERE id = 1",
            "total"
        ),
        10
    );
    assert_eq!(
        int_cell(
            &rt,
            "SELECT SUM(value) AS total FROM ssi_dangerous_chain WHERE id = 2",
            "total"
        ),
        21
    );
    clear_current_connection_id();
}
