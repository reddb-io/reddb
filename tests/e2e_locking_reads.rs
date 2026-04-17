//! Read-path intent-lock wiring (Phase 1.T3).
//!
//! Runs a SELECT and confirms the LockManager's `requests` /
//! `granted` counters ticked — proving the dispatch actually takes
//! the `(Global, IS) → (Collection, IS)` pair before scanning. Also
//! runs the same query with `concurrency.locking.enabled = false` to
//! prove the feature flag is honored.

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
fn select_acquires_intent_shared_locks() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE orders (id INT, amount INT)");
    exec(&rt, "INSERT INTO orders (id, amount) VALUES (1, 100), (2, 200)");

    let before = rt.lock_manager().stats();
    let _ = rt.execute_query("SELECT * FROM orders").unwrap();
    let after = rt.lock_manager().stats();

    // Two acquires per read: (Global, IS) + (Collection, IS).
    assert!(
        after.requests >= before.requests + 2,
        "expected ≥ 2 lock requests, got delta = {}",
        after.requests - before.requests,
    );
    assert!(
        after.granted >= before.granted + 2,
        "expected ≥ 2 granted, got delta = {}",
        after.granted - before.granted,
    );
    // (The LockManager's `active_locks` stat snapshots on grant, not
    // on release, so we don't assert on it here — the `granted`
    // counter proves the acquire path ran.)
}

#[test]
fn disabling_concurrency_locking_skips_acquires() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT)");
    exec(&rt, "SET CONFIG concurrency.locking.enabled = false");

    let before = rt.lock_manager().stats();
    let _ = rt.execute_query("SELECT * FROM t").unwrap();
    let after = rt.lock_manager().stats();

    assert_eq!(
        after.requests, before.requests,
        "lock requests should not increase when locking disabled",
    );
}

#[test]
fn admin_statements_do_not_acquire_locks() {
    let rt = open_runtime();
    let before = rt.lock_manager().stats();
    // SHOW CONFIG / SET TENANT / SHOW TENANT are admin — no intent
    // locks needed. Verifying that the read-path gate doesn't over-
    // reach into admin variants.
    let _ = rt.execute_query("SHOW CONFIG durability.mode").unwrap();
    let _ = rt.execute_query("SHOW TENANT").unwrap();
    let after = rt.lock_manager().stats();
    assert_eq!(after.requests, before.requests);
}
