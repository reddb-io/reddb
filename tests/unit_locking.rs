//! Unit-style coverage for `runtime::locking` — compatibility matrix,
//! RAII release on drop, illegal-escalation rejection, and a
//! 50-thread intent-lock stress test that must not deadlock or
//! timeout.

use std::sync::Arc;
use std::thread;

use reddb::runtime::locking::{AcquireError, LockerGuard, Resource};
use reddb::storage::transaction::lock::{LockManager, LockMode};

fn mgr() -> Arc<LockManager> {
    Arc::new(LockManager::with_defaults())
}

#[test]
fn resource_keys_do_not_collide() {
    assert_ne!(
        Resource::Global.key(),
        Resource::Collection("global".into()).key()
    );
    assert_ne!(
        Resource::Collection("a".into()).key(),
        Resource::Collection("b".into()).key()
    );
}

#[test]
fn guard_acquires_and_releases_on_drop() {
    let m = mgr();
    {
        let mut g = LockerGuard::new(m.clone());
        g.acquire(Resource::Global, LockMode::IntentShared).unwrap();
        g.acquire(
            Resource::Collection("orders".into()),
            LockMode::IntentShared,
        )
        .unwrap();
        assert_eq!(g.held_count(), 2);
    }
    // After drop, a second acquire in X must succeed — means the IS
    // guard really released.
    let mut g = LockerGuard::new(m);
    g.acquire(Resource::Global, LockMode::Exclusive).unwrap();
}

#[test]
fn compatible_intent_locks_do_not_block() {
    let m = mgr();
    let mut a = LockerGuard::new(m.clone());
    let mut b = LockerGuard::new(m);
    a.acquire(Resource::Global, LockMode::IntentExclusive)
        .unwrap();
    // Different txn takes IX on the same resource — IX/IX compatible,
    // acquire must not block indefinitely.
    b.acquire(Resource::Global, LockMode::IntentExclusive)
        .unwrap();
}

#[test]
fn rejects_illegal_escalation() {
    let m = mgr();
    let mut g = LockerGuard::new(m);
    g.acquire(Resource::Global, LockMode::Exclusive).unwrap();
    let err = g
        .acquire(Resource::Global, LockMode::Shared)
        .expect_err("X → S is not an upgrade");
    assert!(matches!(err, AcquireError::IncompatibleEscalation { .. }));
}

#[test]
fn stress_50_threads_random_intent_acquires_no_deadlock() {
    // 50 threads × 10 rounds each, each picking among 8 collections
    // and acquiring IS/IX via `Global → Collection`. Intent locks
    // are mutually compatible, so no thread should ever deadlock or
    // timeout.
    let m = mgr();
    let mut handles = Vec::new();
    for thread_no in 0..50 {
        let mm = m.clone();
        handles.push(thread::spawn(move || {
            for round in 0..10 {
                let coll = (thread_no * 7 + round) % 8;
                let mode = if (thread_no + round) % 2 == 0 {
                    LockMode::IntentShared
                } else {
                    LockMode::IntentExclusive
                };
                let mut g = LockerGuard::new(mm.clone());
                g.acquire(Resource::Global, mode).unwrap();
                g.acquire(Resource::Collection(format!("c{coll}")), mode)
                    .unwrap();
                // Drop releases both.
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}
