//! Transaction isolation level acceptance tests.
//!
//! reddb today runs all transactions under snapshot isolation. The
//! parser accepts READ UNCOMMITTED / READ COMMITTED / REPEATABLE
//! READ / SNAPSHOT as PG-compatibility no-ops and rejects
//! SERIALIZABLE explicitly rather than silently downgrading.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn try_exec(rt: &RedDBRuntime, sql: &str) -> Result<(), String> {
    rt.execute_query(sql).map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn begin_accepts_read_committed() {
    let rt = rt();
    set_current_connection_id(9901);
    try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .expect("READ COMMITTED should be accepted");
    try_exec(&rt, "COMMIT").expect("COMMIT should close the tx");
    clear_current_connection_id();
}

#[test]
fn begin_accepts_repeatable_read() {
    let rt = rt();
    set_current_connection_id(9902);
    try_exec(&rt, "BEGIN ISOLATION LEVEL REPEATABLE READ")
        .expect("REPEATABLE READ should be accepted");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

#[test]
fn begin_accepts_snapshot() {
    let rt = rt();
    set_current_connection_id(9903);
    try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL SNAPSHOT")
        .expect("SNAPSHOT should be accepted");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

#[test]
fn begin_rejects_serializable_with_clear_message() {
    let rt = rt();
    set_current_connection_id(9904);
    let err = try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE").unwrap_err();
    assert!(
        err.contains("SERIALIZABLE"),
        "error should mention SERIALIZABLE: {err}"
    );
    assert!(
        err.to_ascii_lowercase().contains("not yet supported")
            || err.to_ascii_lowercase().contains("not supported"),
        "error should say unsupported: {err}"
    );
    clear_current_connection_id();
}

#[test]
fn start_transaction_isolation_level_is_accepted() {
    let rt = rt();
    set_current_connection_id(9905);
    try_exec(&rt, "START TRANSACTION ISOLATION LEVEL READ UNCOMMITTED")
        .expect("READ UNCOMMITTED should be accepted (upgraded to snapshot)");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}
