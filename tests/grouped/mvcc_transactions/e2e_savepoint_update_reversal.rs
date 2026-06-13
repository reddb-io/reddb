//! Savepoint-aware UPDATE reversal.

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

fn red_entity_id(rt: &RedDBRuntime, table: &str) -> u64 {
    let result = rt
        .execute_query(&format!("SELECT red_entity_id FROM {table} WHERE id = 1"))
        .expect("select red_entity_id");
    match result.result.records[0].get("red_entity_id") {
        Some(Value::UnsignedInteger(id)) => *id,
        Some(Value::Integer(id)) => *id as u64,
        other => panic!("expected red_entity_id, got {other:?}"),
    }
}

fn label_for(rt: &RedDBRuntime, table: &str, red_entity_id: u64) -> String {
    let result = rt
        .execute_query(&format!(
            "SELECT label FROM {table} WHERE red_entity_id = {red_entity_id}"
        ))
        .expect("select label");
    match result.result.records[0].get("label") {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected label, got {other:?}"),
    }
}

#[test]
fn rollback_to_savepoint_restores_pre_update_value() {
    let rt = rt();
    set_current_connection_id(7701);

    exec(&rt, "CREATE TABLE sp_upd (id INT, label TEXT)");
    exec(&rt, "INSERT INTO sp_upd (id, label) VALUES (1, 'before')");
    let eid = red_entity_id(&rt, "sp_upd");

    exec(&rt, "BEGIN");
    exec(&rt, "SAVEPOINT sp1");
    exec(
        &rt,
        &format!("UPDATE sp_upd SET label = 'after' WHERE red_entity_id = {eid}"),
    );
    exec(&rt, "ROLLBACK TO SAVEPOINT sp1");
    exec(&rt, "COMMIT");

    assert_eq!(label_for(&rt, "sp_upd", eid), "before");

    clear_current_connection_id();
}

#[test]
fn nested_savepoints_restore_the_right_update_version() {
    let rt = rt();
    set_current_connection_id(7702);

    exec(&rt, "CREATE TABLE sp_nested_upd (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO sp_nested_upd (id, label) VALUES (1, 'base')",
    );
    let eid = red_entity_id(&rt, "sp_nested_upd");

    exec(&rt, "BEGIN");
    exec(
        &rt,
        &format!("UPDATE sp_nested_upd SET label = 'one' WHERE red_entity_id = {eid}"),
    );
    exec(&rt, "SAVEPOINT sp1");
    exec(
        &rt,
        &format!("UPDATE sp_nested_upd SET label = 'two' WHERE red_entity_id = {eid}"),
    );
    exec(&rt, "SAVEPOINT sp2");
    exec(
        &rt,
        &format!("UPDATE sp_nested_upd SET label = 'three' WHERE red_entity_id = {eid}"),
    );
    exec(&rt, "ROLLBACK TO SAVEPOINT sp2");
    assert_eq!(label_for(&rt, "sp_nested_upd", eid), "two");
    exec(&rt, "ROLLBACK TO SAVEPOINT sp1");
    assert_eq!(label_for(&rt, "sp_nested_upd", eid), "one");
    exec(&rt, "COMMIT");
    assert_eq!(label_for(&rt, "sp_nested_upd", eid), "one");

    clear_current_connection_id();
}

#[test]
fn release_savepoint_preserves_update_work() {
    let rt = rt();
    set_current_connection_id(7703);

    exec(&rt, "CREATE TABLE sp_release_upd (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO sp_release_upd (id, label) VALUES (1, 'base')",
    );
    let eid = red_entity_id(&rt, "sp_release_upd");

    exec(&rt, "BEGIN");
    exec(&rt, "SAVEPOINT sp1");
    exec(
        &rt,
        &format!("UPDATE sp_release_upd SET label = 'kept' WHERE red_entity_id = {eid}"),
    );
    exec(&rt, "RELEASE SAVEPOINT sp1");
    assert_eq!(label_for(&rt, "sp_release_upd", eid), "kept");
    exec(&rt, "COMMIT");
    assert_eq!(label_for(&rt, "sp_release_upd", eid), "kept");

    clear_current_connection_id();
}
