//! MVCC table-row DELETE tombstones.

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

fn label_for(rt: &RedDBRuntime, table: &str, red_entity_id: u64) -> Option<String> {
    let result = rt
        .execute_query(&format!(
            "SELECT label FROM {table} WHERE red_entity_id = {red_entity_id}"
        ))
        .expect("select label");
    result
        .result
        .records
        .first()
        .and_then(|record| match record.get("label") {
            Some(Value::Text(value)) => Some(value.to_string()),
            other => panic!("expected label or empty result, got {other:?}"),
        })
}

#[test]
fn delete_tombstone_preserves_old_snapshot_and_hides_new_snapshot() {
    let rt = rt();
    set_current_connection_id(43801);

    exec(&rt, "CREATE TABLE mvcc_delete_snap (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO mvcc_delete_snap (id, label) VALUES (1, 'base')",
    );
    let eid = red_entity_id(&rt, "mvcc_delete_snap");

    exec(&rt, "BEGIN");
    assert_eq!(
        label_for(&rt, "mvcc_delete_snap", eid).as_deref(),
        Some("base")
    );

    set_current_connection_id(43802);
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_snap WHERE red_entity_id = {eid}"),
    );
    assert_eq!(label_for(&rt, "mvcc_delete_snap", eid), None);

    set_current_connection_id(43801);
    assert_eq!(
        label_for(&rt, "mvcc_delete_snap", eid).as_deref(),
        Some("base")
    );
    exec(&rt, "COMMIT");

    set_current_connection_id(43803);
    assert_eq!(label_for(&rt, "mvcc_delete_snap", eid), None);

    clear_current_connection_id();
}

#[test]
fn explicit_delete_is_invisible_to_other_transactions_until_commit() {
    let rt = rt();
    set_current_connection_id(43811);

    exec(&rt, "CREATE TABLE mvcc_delete_tx (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO mvcc_delete_tx (id, label) VALUES (1, 'base')",
    );
    let eid = red_entity_id(&rt, "mvcc_delete_tx");

    set_current_connection_id(43812);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_tx WHERE red_entity_id = {eid}"),
    );
    assert_eq!(label_for(&rt, "mvcc_delete_tx", eid), None);

    set_current_connection_id(43813);
    assert_eq!(
        label_for(&rt, "mvcc_delete_tx", eid).as_deref(),
        Some("base")
    );

    set_current_connection_id(43812);
    exec(&rt, "COMMIT");

    set_current_connection_id(43814);
    assert_eq!(label_for(&rt, "mvcc_delete_tx", eid), None);

    clear_current_connection_id();
}

#[test]
fn rollback_of_staged_delete_leaves_row_visible() {
    let rt = rt();
    set_current_connection_id(43821);

    exec(
        &rt,
        "CREATE TABLE mvcc_delete_rollback (id INT, label TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_delete_rollback (id, label) VALUES (1, 'base')",
    );
    let eid = red_entity_id(&rt, "mvcc_delete_rollback");

    exec(&rt, "BEGIN");
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_rollback WHERE red_entity_id = {eid}"),
    );
    assert_eq!(label_for(&rt, "mvcc_delete_rollback", eid), None);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(43822);
    assert_eq!(
        label_for(&rt, "mvcc_delete_rollback", eid).as_deref(),
        Some("base")
    );

    clear_current_connection_id();
}
