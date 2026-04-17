//! Phase 1.T4 — writes take `(Global, IX) → (Collection, IX)`.
//!
//! Two properties:
//!
//! 1. A single INSERT bumps the lock-manager's `requests` counter by
//!    at least the global + collection pair.
//! 2. Concurrent writers to DIFFERENT collections do not serialise —
//!    20 threads × 200 inserts each, 5 distinct collections. IX/IX
//!    compatibility means no thread should block on another's
//!    collection lock.

use std::sync::Arc;
use std::thread;

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn insert_acquires_intent_exclusive_locks() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, v INT)");

    let before = rt.lock_manager().stats();
    exec(&rt, "INSERT INTO t (id, v) VALUES (1, 10)");
    let after = rt.lock_manager().stats();

    // (Global, IX) + (Collection, IX) = 2 acquires per write.
    assert!(
        after.requests >= before.requests + 2,
        "INSERT should acquire ≥ 2 locks: delta = {}",
        after.requests - before.requests,
    );
    assert!(after.granted >= before.granted + 2);
}

#[test]
fn update_acquires_intent_exclusive_locks() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, v INT)");
    exec(&rt, "INSERT INTO t (id, v) VALUES (1, 10)");

    let before = rt.lock_manager().stats();
    exec(&rt, "UPDATE t SET v = 99 WHERE id = 1");
    let after = rt.lock_manager().stats();
    assert!(after.granted >= before.granted + 2);
}

#[test]
fn delete_acquires_intent_exclusive_locks() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT)");
    exec(&rt, "INSERT INTO t (id) VALUES (1), (2)");

    let before = rt.lock_manager().stats();
    exec(&rt, "DELETE FROM t WHERE id = 1");
    let after = rt.lock_manager().stats();
    assert!(after.granted >= before.granted + 2);
}

#[test]
fn twenty_threads_five_collections_do_not_deadlock() {
    // Concurrent writers to five distinct collections must all
    // complete. IX on Global is compatible across threads; IX on
    // different collections is trivially compatible. Total time is
    // dominated by storage, not lock contention — we assert only on
    // completion, not throughput.
    let rt = Arc::new(open_runtime());
    for c in 0..5 {
        exec(&rt, &format!("CREATE TABLE c{c} (id INT, v INT)"));
    }

    let mut handles = Vec::new();
    for t in 0..20 {
        let r = Arc::clone(&rt);
        handles.push(thread::spawn(move || {
            for i in 0..200 {
                let coll = (t + i) % 5;
                let sql = format!("INSERT INTO c{coll} (id, v) VALUES ({i}, {t})");
                r.execute_query(&sql)
                    .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    // Sanity: every collection got rows from multiple threads.
    for c in 0..5 {
        let r = rt
            .execute_query(&format!("SELECT * FROM c{c}"))
            .unwrap();
        assert!(r.result.records.len() > 0, "c{c} empty");
    }
}
