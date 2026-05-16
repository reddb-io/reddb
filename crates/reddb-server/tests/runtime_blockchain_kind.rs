//! Issue #523 — runtime-level coverage for `KIND blockchain` collections.
//!
//! Exercises kind persistence, auto-genesis at creation, reserved-column
//! auto-fill on INSERT, mutate-gate rejection on UPDATE/DELETE, and the
//! "other kinds unaffected" invariant. Hash semantics live in
//! `storage::blockchain` and `runtime::blockchain_kind` unit tests — this
//! file is the integration glue.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn select_all(rt: &RedDBRuntime, name: &str) -> RuntimeQueryResult {
    rt.execute_query(&format!("SELECT * FROM {name}"))
        .expect("select")
}

fn height_at(res: &RuntimeQueryResult, row: usize) -> u64 {
    match res.result.records[row].get("block_height") {
        Some(Value::UnsignedInteger(v)) => *v,
        Some(Value::Integer(v)) => *v as u64,
        other => panic!("block_height missing/wrong at row {row}: {other:?}"),
    }
}

fn blob_at(res: &RuntimeQueryResult, row: usize, column: &str) -> Vec<u8> {
    match res.result.records[row].get(column) {
        Some(Value::Blob(b)) => b.clone(),
        other => panic!("{column} blob missing at row {row}: {other:?}"),
    }
}

fn sort_by_height(res: &mut RuntimeQueryResult) {
    res.result.records.sort_by_key(|r| match r.get("block_height") {
        Some(Value::UnsignedInteger(v)) => *v as i64,
        Some(Value::Integer(v)) => *v,
        _ => i64::MAX,
    });
}

#[test]
fn create_collection_kind_blockchain_persists_kind_and_inserts_genesis() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create blockchain collection");

    let res = select_all(&rt, "audit_log");
    assert_eq!(res.result.records.len(), 1, "genesis row auto-inserted");
    assert_eq!(height_at(&res, 0), 0, "genesis at height 0");
    assert_eq!(blob_at(&res, 0, "prev_hash"), vec![0u8; 32]);
    assert_eq!(blob_at(&res, 0, "hash").len(), 32);
}

#[test]
fn insert_into_blockchain_appends_and_chains_hashes() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    rt.execute_query("INSERT INTO audit_log (actor, action) VALUES ('alice', 'login')")
        .expect("insert 1");
    rt.execute_query("INSERT INTO audit_log (actor, action) VALUES ('bob', 'logout')")
        .expect("insert 2");

    let mut res = select_all(&rt, "audit_log");
    sort_by_height(&mut res);
    assert_eq!(res.result.records.len(), 3, "genesis + 2 inserts");

    assert_eq!(blob_at(&res, 0, "prev_hash"), vec![0u8; 32]);
    assert_eq!(blob_at(&res, 1, "prev_hash"), blob_at(&res, 0, "hash"));
    assert_eq!(blob_at(&res, 2, "prev_hash"), blob_at(&res, 1, "hash"));
    assert_ne!(blob_at(&res, 0, "hash"), blob_at(&res, 1, "hash"));
    assert_ne!(blob_at(&res, 1, "hash"), blob_at(&res, 2, "hash"));
    assert_eq!(height_at(&res, 0), 0);
    assert_eq!(height_at(&res, 1), 1);
    assert_eq!(height_at(&res, 2), 2);
}

#[test]
fn update_on_blockchain_returns_immutable_error() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    rt.execute_query("INSERT INTO audit_log (actor) VALUES ('alice')")
        .expect("insert");

    let err = rt
        .execute_query("UPDATE audit_log SET actor = 'mallory' WHERE actor = 'alice'")
        .expect_err("update must be rejected");
    match err {
        RedDBError::InvalidOperation(msg) => {
            assert!(
                msg.contains("BlockchainCollectionImmutable"),
                "expected BlockchainCollectionImmutable in {msg}"
            );
        }
        other => panic!("expected InvalidOperation, got {other:?}"),
    }
}

#[test]
fn delete_on_blockchain_returns_immutable_error() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    rt.execute_query("INSERT INTO audit_log (actor) VALUES ('alice')")
        .expect("insert");

    let err = rt
        .execute_query("DELETE FROM audit_log WHERE actor = 'alice'")
        .expect_err("delete must be rejected");
    match err {
        RedDBError::InvalidOperation(msg) => {
            assert!(
                msg.contains("BlockchainCollectionImmutable"),
                "expected BlockchainCollectionImmutable in {msg}"
            );
        }
        other => panic!("expected InvalidOperation, got {other:?}"),
    }
}

#[test]
fn user_supplied_reserved_columns_are_overwritten_by_engine() {
    let rt = rt();
    rt.execute_query("CREATE COLLECTION audit_log KIND blockchain")
        .expect("create");
    rt.execute_query("INSERT INTO audit_log (actor, block_height) VALUES ('alice', 9999)")
        .expect("insert");

    let mut res = select_all(&rt, "audit_log");
    sort_by_height(&mut res);
    let last = res.result.records.len() - 1;
    assert_eq!(
        height_at(&res, last),
        1,
        "engine recomputes block_height, ignores user-supplied 9999"
    );
}

#[test]
fn non_blockchain_kinds_allow_update_and_delete() {
    let rt = rt();
    rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'alice')")
        .expect("insert");
    rt.execute_query("UPDATE users SET name = 'mallory' WHERE id = 1")
        .expect("update on non-blockchain table must succeed");
    rt.execute_query("DELETE FROM users WHERE id = 1")
        .expect("delete on non-blockchain table must succeed");
}
