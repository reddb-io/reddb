//! MVCC table-row DELETE tombstones.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn rid(rt: &RedDBRuntime, table: &str) -> u64 {
    let result = rt
        .execute_query(&format!("SELECT rid FROM {table} WHERE id = 1"))
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

fn scanned_ids(rt: &RedDBRuntime, table: &str) -> Vec<i64> {
    rt.execute_query(&format!("SELECT id FROM {table} ORDER BY id"))
        .expect("scan ids")
        .result
        .records
        .iter()
        .map(|record| match record.get("id") {
            Some(Value::Integer(id)) => *id,
            Some(Value::UnsignedInteger(id)) => *id as i64,
            other => panic!("expected id, got {other:?}"),
        })
        .collect()
}

#[test]
fn cached_cross_collection_reads_follow_sealed_delete_rollback_and_vacuum() {
    let rt = rt();
    set_current_connection_id(43871);
    exec(&rt, "CREATE TABLE mvcc_cached_delete (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO mvcc_cached_delete (id, label) VALUES (1, 'base')",
    );
    let id = reddb::storage::EntityId::new(rid(&rt, "mvcc_cached_delete"));
    let store = rt.db().store();
    let manager = store.get_collection("mvcc_cached_delete").expect("source");
    manager.force_seal().expect("sealed source");
    assert_eq!(store.get_any(id).expect("warm cache").1.xmax, 0);

    exec(&rt, "BEGIN");
    exec(&rt, "DELETE FROM mvcc_cached_delete WHERE id = 1");
    let deleted = manager.get(id).expect("retained old version");
    assert_ne!(deleted.xmax, 0);
    assert_eq!(
        store.get_any(id).expect("cached current header").1.xmax,
        deleted.xmax
    );
    exec(&rt, "ROLLBACK");
    let revived = store.get_any(id).expect("cached revived version").1;
    assert_eq!(revived.xmax, 0);
    assert_eq!(
        revived.data.as_row().expect("row").get_field("label"),
        Some(&Value::text("base"))
    );
    assert_eq!(scanned_ids(&rt, "mvcc_cached_delete"), [1]);

    exec(&rt, "DELETE FROM mvcc_cached_delete WHERE id = 1");
    let deleted = manager.get(id).expect("committed retained version");
    assert_ne!(deleted.xmax, 0);
    assert_eq!(
        store.get_any(id).expect("cached committed header").1.xmax,
        deleted.xmax
    );
    assert!(scanned_ids(&rt, "mvcc_cached_delete").is_empty());
    exec(&rt, "VACUUM mvcc_cached_delete");
    assert!(manager.get(id).is_none());
    assert!(
        store.get_any(id).is_none(),
        "a cached copy must not revive reclaimed history"
    );
    clear_current_connection_id();
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
    let eid = rid(&rt, "mvcc_delete_snap");

    exec(&rt, "BEGIN");
    assert_eq!(
        label_for(&rt, "mvcc_delete_snap", eid).as_deref(),
        Some("base")
    );

    set_current_connection_id(43802);
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_snap WHERE rid = {eid}"),
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
fn table_scan_preserves_snapshot_visibility_for_tombstoned_rows() {
    let rt = rt();
    set_current_connection_id(43831);

    exec(&rt, "CREATE TABLE mvcc_scan_resolver (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO mvcc_scan_resolver (id, label) VALUES (1, 'keep'), (2, 'delete')",
    );

    exec(&rt, "BEGIN");
    assert_eq!(scanned_ids(&rt, "mvcc_scan_resolver"), vec![1, 2]);

    set_current_connection_id(43832);
    exec(&rt, "DELETE FROM mvcc_scan_resolver WHERE id = 2");
    assert_eq!(scanned_ids(&rt, "mvcc_scan_resolver"), vec![1]);

    set_current_connection_id(43831);
    assert_eq!(scanned_ids(&rt, "mvcc_scan_resolver"), vec![1, 2]);
    exec(&rt, "COMMIT");

    set_current_connection_id(43833);
    assert_eq!(scanned_ids(&rt, "mvcc_scan_resolver"), vec![1]);

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
    let eid = rid(&rt, "mvcc_delete_tx");

    set_current_connection_id(43812);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_tx WHERE rid = {eid}"),
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
    let eid = rid(&rt, "mvcc_delete_rollback");

    exec(&rt, "BEGIN");
    exec(
        &rt,
        &format!("DELETE FROM mvcc_delete_rollback WHERE rid = {eid}"),
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
