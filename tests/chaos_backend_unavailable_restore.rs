//! Chaos test: restore against an empty / missing backend
//! (PLAN.md Phase 8 slice).
//!
//! Validates the fail-closed contract: when no snapshot manifest is
//! published under the configured prefix, `PointInTimeRecovery` must
//! return `BackendError::NotFound` with a structured message — not
//! crash, not silently produce an empty DB. This is the exact case
//! a freshly-rotated bucket or misconfigured `RED_BACKEND` would hit.

use reddb::storage::backend::{BackendError, LocalBackend};
use reddb::storage::wal::PointInTimeRecovery;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "reddb-chaos-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn restore_against_empty_backend_fails_with_not_found() {
    // Spin up an empty work tree — `snapshots/` and `wal/` exist but
    // contain nothing. Whoever booted this misconfigured the
    // backend (typo in RED_REMOTE_KEY, fresh bucket, deleted state).
    let work = temp_dir("backend-empty");
    let snapshot_dir = work.join("snapshots");
    let wal_dir = work.join("wal");
    let restore_path = work.join("restore").join("data.rdb");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    std::fs::create_dir_all(&wal_dir).unwrap();

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        wal_dir.to_string_lossy().to_string(),
    );

    let err = recovery
        .restore_to(0, &restore_path)
        .expect_err("restore against empty backend must fail closed");
    match err {
        BackendError::NotFound(msg) => {
            assert!(
                msg.to_lowercase().contains("snapshot"),
                "error must point at the missing snapshot; got: {msg}"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(
        !restore_path.exists(),
        "no destination DB must be created when restore fails"
    );

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn restore_against_missing_prefix_fails_with_not_found_or_transport() {
    // A prefix that doesn't even exist on the backend (no
    // snapshots/ folder created). LocalBackend treats this as
    // empty list; the recovery wrapper must still surface a clean
    // error, not panic.
    let work = temp_dir("missing-prefix");
    let restore_path = work.join("restore").join("data.rdb");

    let bogus_snapshot_prefix = work.join("does-not-exist").join("snapshots");
    let bogus_wal_prefix = work.join("does-not-exist").join("wal");

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        bogus_snapshot_prefix.to_string_lossy().to_string(),
        bogus_wal_prefix.to_string_lossy().to_string(),
    );

    let res = recovery.restore_to(0, &restore_path);
    assert!(
        res.is_err(),
        "restore against missing prefix must surface an error"
    );
    let err = res.unwrap_err();
    match err {
        BackendError::NotFound(_) | BackendError::Transport(_) => {}
        other => panic!("expected NotFound or Transport, got {other:?}"),
    }
    assert!(!restore_path.exists());

    let _ = std::fs::remove_dir_all(&work);
}
