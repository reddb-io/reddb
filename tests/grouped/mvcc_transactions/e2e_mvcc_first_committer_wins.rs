//! First-committer-wins conflicts for table-row logical identities.

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

fn rid(rt: &RedDBRuntime, table: &str, id: i64) -> u64 {
    let result = rt
        .execute_query(&format!("SELECT rid FROM {table} WHERE id = {id}"))
        .expect("select rid");
    match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(id)) => *id,
        Some(Value::Integer(id)) => *id as u64,
        other => panic!("expected rid, got {other:?}"),
    }
}

fn label_for(rt: &RedDBRuntime, table: &str, rid: u64) -> Option<String> {
    let result = rt
        .execute_query(&format!("SELECT label FROM {table} WHERE rid = {rid}"))
        .expect("select label");
    result
        .result
        .records
        .first()
        .map(|record| match record.get("label") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected label or empty result, got {other:?}"),
        })
}

fn assert_conflict(message: &str) {
    assert!(
        message.contains("serialization conflict"),
        "expected serialization conflict, got {message}"
    );
}

#[test]
fn concurrent_updates_same_logical_row_conflict_on_second_commit() {
    let rt = rt();
    set_current_connection_id(43901);
    exec(&rt, "CREATE TABLE fcw_update (id INT, label TEXT)");
    exec(&rt, "INSERT INTO fcw_update (id, label) VALUES (1, 'base')");
    let eid = rid(&rt, "fcw_update", 1);

    set_current_connection_id(43902);
    exec(&rt, "BEGIN");
    set_current_connection_id(43903);
    exec(&rt, "BEGIN");

    set_current_connection_id(43902);
    exec(
        &rt,
        &format!("UPDATE fcw_update SET label = 'first' WHERE rid = {eid}"),
    );
    set_current_connection_id(43903);
    exec(
        &rt,
        &format!("UPDATE fcw_update SET label = 'second' WHERE rid = {eid}"),
    );

    set_current_connection_id(43902);
    exec(&rt, "COMMIT");
    set_current_connection_id(43903);
    assert_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(43904);
    assert_eq!(label_for(&rt, "fcw_update", eid).as_deref(), Some("first"));
    clear_current_connection_id();
}

#[test]
fn concurrent_updates_different_logical_rows_both_commit() {
    let rt = rt();
    set_current_connection_id(43911);
    exec(&rt, "CREATE TABLE fcw_distinct (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO fcw_distinct (id, label) VALUES (1, 'one'), (2, 'two')",
    );
    let eid1 = rid(&rt, "fcw_distinct", 1);
    let eid2 = rid(&rt, "fcw_distinct", 2);

    set_current_connection_id(43912);
    exec(&rt, "BEGIN");
    set_current_connection_id(43913);
    exec(&rt, "BEGIN");

    set_current_connection_id(43912);
    exec(
        &rt,
        &format!("UPDATE fcw_distinct SET label = 'first' WHERE rid = {eid1}"),
    );
    set_current_connection_id(43913);
    exec(
        &rt,
        &format!("UPDATE fcw_distinct SET label = 'second' WHERE rid = {eid2}"),
    );

    set_current_connection_id(43912);
    exec(&rt, "COMMIT");
    set_current_connection_id(43913);
    exec(&rt, "COMMIT");

    set_current_connection_id(43914);
    assert_eq!(
        label_for(&rt, "fcw_distinct", eid1).as_deref(),
        Some("first")
    );
    assert_eq!(
        label_for(&rt, "fcw_distinct", eid2).as_deref(),
        Some("second")
    );
    clear_current_connection_id();
}

#[test]
fn update_then_delete_same_logical_row_conflicts() {
    let rt = rt();
    set_current_connection_id(43921);
    exec(&rt, "CREATE TABLE fcw_update_delete (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO fcw_update_delete (id, label) VALUES (1, 'base')",
    );
    let eid = rid(&rt, "fcw_update_delete", 1);

    set_current_connection_id(43922);
    exec(&rt, "BEGIN");
    set_current_connection_id(43923);
    exec(&rt, "BEGIN");

    set_current_connection_id(43922);
    exec(
        &rt,
        &format!("UPDATE fcw_update_delete SET label = 'kept' WHERE rid = {eid}"),
    );
    set_current_connection_id(43923);
    exec(
        &rt,
        &format!("DELETE FROM fcw_update_delete WHERE rid = {eid}"),
    );

    set_current_connection_id(43922);
    exec(&rt, "COMMIT");
    set_current_connection_id(43923);
    assert_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(43924);
    assert_eq!(
        label_for(&rt, "fcw_update_delete", eid).as_deref(),
        Some("kept")
    );
    clear_current_connection_id();
}

#[test]
fn delete_then_update_same_logical_row_conflicts() {
    let rt = rt();
    set_current_connection_id(43931);
    exec(&rt, "CREATE TABLE fcw_delete_update (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO fcw_delete_update (id, label) VALUES (1, 'base')",
    );
    let eid = rid(&rt, "fcw_delete_update", 1);

    set_current_connection_id(43932);
    exec(&rt, "BEGIN");
    set_current_connection_id(43933);
    exec(&rt, "BEGIN");

    set_current_connection_id(43932);
    exec(
        &rt,
        &format!("DELETE FROM fcw_delete_update WHERE rid = {eid}"),
    );
    set_current_connection_id(43933);
    exec(
        &rt,
        &format!("UPDATE fcw_delete_update SET label = 'stale' WHERE rid = {eid}"),
    );

    set_current_connection_id(43932);
    exec(&rt, "COMMIT");
    set_current_connection_id(43933);
    assert_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(43934);
    assert_eq!(label_for(&rt, "fcw_delete_update", eid), None);
    clear_current_connection_id();
}

#[test]
fn stale_transaction_conflicts_with_autocommit_update() {
    let rt = rt();
    set_current_connection_id(43941);
    exec(&rt, "CREATE TABLE fcw_autocommit (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO fcw_autocommit (id, label) VALUES (1, 'base')",
    );
    let eid = rid(&rt, "fcw_autocommit", 1);

    set_current_connection_id(43942);
    exec(&rt, "BEGIN");

    set_current_connection_id(43943);
    exec(
        &rt,
        &format!("UPDATE fcw_autocommit SET label = 'auto' WHERE rid = {eid}"),
    );

    set_current_connection_id(43942);
    exec(
        &rt,
        &format!("UPDATE fcw_autocommit SET label = 'stale' WHERE rid = {eid}"),
    );
    assert_conflict(&exec_err(&rt, "COMMIT"));

    set_current_connection_id(43944);
    assert_eq!(
        label_for(&rt, "fcw_autocommit", eid).as_deref(),
        Some("auto")
    );
    clear_current_connection_id();
}
