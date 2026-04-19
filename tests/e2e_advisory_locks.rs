//! Advisory locks end-to-end tests (T6 / PG gap item #8b).
//!
//! Exercises `pg_try_advisory_lock`, `pg_advisory_unlock`,
//! `pg_advisory_unlock_all` via SQL. `pg_advisory_lock` (blocking)
//! has its own threaded test that covers contention across two
//! simulated connections.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"),
    )
}

fn eval_bool(rt: &RedDBRuntime, sql: &str) -> bool {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    let rec = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("{sql}: no record"));
    let (_, v) = rec
        .values
        .iter()
        .next()
        .unwrap_or_else(|| panic!("{sql}: empty record"));
    match v {
        Value::Boolean(b) => *b,
        other => panic!("{sql}: expected bool, got {other:?}"),
    }
}

#[test]
fn try_advisory_lock_succeeds_then_blocks_other_conn() {
    let rt = rt();

    set_current_connection_id(501);
    assert!(
        eval_bool(&rt, "SELECT pg_try_advisory_lock(42)"),
        "first conn acquires"
    );
    // Re-entrant on same conn.
    assert!(
        eval_bool(&rt, "SELECT pg_try_advisory_lock(42)"),
        "same conn is reentrant"
    );

    set_current_connection_id(502);
    assert!(
        !eval_bool(&rt, "SELECT pg_try_advisory_lock(42)"),
        "other conn must fail"
    );

    set_current_connection_id(501);
    assert!(
        eval_bool(&rt, "SELECT pg_advisory_unlock(42)"),
        "owner releases"
    );

    set_current_connection_id(502);
    assert!(
        eval_bool(&rt, "SELECT pg_try_advisory_lock(42)"),
        "second conn acquires after release"
    );
    eval_bool(&rt, "SELECT pg_advisory_unlock(42)");

    clear_current_connection_id();
}

#[test]
fn unlock_by_non_owner_returns_false() {
    let rt = rt();

    set_current_connection_id(601);
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(7)"));

    set_current_connection_id(602);
    assert!(
        !eval_bool(&rt, "SELECT pg_advisory_unlock(7)"),
        "non-owner unlock is a no-op bool=false"
    );

    set_current_connection_id(601);
    assert!(eval_bool(&rt, "SELECT pg_advisory_unlock(7)"));
    clear_current_connection_id();
}

#[test]
fn unlock_all_drops_every_held_lock() {
    let rt = rt();
    set_current_connection_id(701);
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(10)"));
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(20)"));
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(30)"));

    let result = rt
        .execute_query("SELECT pg_advisory_unlock_all()")
        .unwrap();
    match result.result.records[0].values.values().next().unwrap() {
        Value::Integer(n) => assert_eq!(*n, 3, "released 3 locks"),
        other => panic!("expected integer count, got {other:?}"),
    }

    // And another conn can now take them.
    set_current_connection_id(702);
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(10)"));
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(20)"));
    let _ = rt.execute_query("SELECT pg_advisory_unlock_all()").unwrap();

    clear_current_connection_id();
}

#[test]
fn blocking_advisory_lock_waits_for_release() {
    let rt = rt();

    set_current_connection_id(801);
    assert!(eval_bool(&rt, "SELECT pg_try_advisory_lock(99)"));

    // Spawn a thread that pins conn 802 and calls the blocking
    // variant. It should hang until the main thread unlocks.
    let rt_c = Arc::clone(&rt);
    let handle = thread::spawn(move || {
        set_current_connection_id(802);
        // Blocks inside pg_advisory_lock until 801 releases.
        let _ = rt_c
            .execute_query("SELECT pg_advisory_lock(99)")
            .expect("blocking acquire");
        // Confirm we now own it.
        let owned = rt_c
            .execute_query("SELECT pg_advisory_unlock(99)")
            .expect("unlock after acquire");
        match owned.result.records[0].values.values().next().unwrap() {
            Value::Boolean(b) => *b,
            other => panic!("expected bool, got {other:?}"),
        }
    });

    // Give the worker some time to enter pg_advisory_lock.
    thread::sleep(Duration::from_millis(100));
    // Release on main; worker should proceed.
    set_current_connection_id(801);
    assert!(eval_bool(&rt, "SELECT pg_advisory_unlock(99)"));

    let worker_owned = handle
        .join()
        .expect("worker thread")
        .then_some(true)
        .unwrap_or(false);
    assert!(worker_owned, "worker acquired after main released");

    clear_current_connection_id();
}
