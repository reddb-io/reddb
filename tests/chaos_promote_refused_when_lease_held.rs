//! Chaos test: promotion refusal under lease contention
//! (PLAN.md Phase 11.6 + 5.2 + 8 slice).
//!
//! Two would-be primaries race for the same database key. One holds
//! the lease (fresh acquire, TTL still alive); the second tries to
//! promote. The `LeaseStore::try_acquire` path the promotion handler
//! relies on must report `Held` with the existing holder's identity
//! so the operator sees who's blocking the takeover instead of a
//! generic 409.

use reddb::replication::lease::{LeaseError, LeaseStore};
use reddb::storage::backend::LocalBackend;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_prefix(tag: &str) -> String {
    let mut p = PathBuf::from(std::env::temp_dir());
    p.push(format!(
        "reddb-chaos-promote-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn second_promoter_is_refused_while_first_lease_alive() {
    let prefix = temp_prefix("contended");
    let store = LeaseStore::new(Arc::new(LocalBackend)).with_prefix(prefix.clone());

    // Holder A acquires a 60s lease.
    let lease_a = store
        .try_acquire("main", "writer-a", 60_000)
        .expect("first acquire");
    assert_eq!(lease_a.holder_id, "writer-a");
    assert_eq!(lease_a.generation, 1);

    // Holder B (the would-be promoter) tries while A's lease is fresh.
    let err = store
        .try_acquire("main", "writer-b", 60_000)
        .expect_err("second acquire must fail");
    match err {
        LeaseError::Held { current, .. } => {
            assert_eq!(current.holder_id, "writer-a");
            assert_eq!(current.generation, 1);
        }
        other => panic!("expected LeaseError::Held with writer-a, got {other:?}"),
    }

    // Sanity: holder A can still refresh — the contention probe must
    // not have stomped the lease object.
    let refreshed = store.refresh(&lease_a, 60_000).expect("refresh");
    assert_eq!(refreshed.generation, 1);
    assert!(refreshed.expires_at_ms >= lease_a.expires_at_ms);

    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn promote_after_lease_expiry_bumps_generation() {
    let prefix = temp_prefix("expired-takeover");
    let store = LeaseStore::new(Arc::new(LocalBackend)).with_prefix(prefix.clone());

    // Original holder takes a 1ms lease; we sleep past expiry.
    let lease_a = store
        .try_acquire("main", "writer-a", 1)
        .expect("first acquire");
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Promoter B claims it — generation must bump so any stale write
    // from A can be detected by gen mismatch on the manifest.
    let lease_b = store
        .try_acquire("main", "writer-b", 60_000)
        .expect("expired lease must be poachable");
    assert_eq!(lease_b.holder_id, "writer-b");
    assert_eq!(lease_b.generation, lease_a.generation + 1);

    let _ = std::fs::remove_dir_all(&prefix);
}
