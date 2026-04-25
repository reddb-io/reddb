//! Chaos test: replica LSN-collision divergence fail-closed
//! (PLAN.md Phase 11.5 + 8 slice).
//!
//! Same LSN, different payload bytes is the strongest signal a replica
//! has of primary corruption or split-brain. Apply must fail closed.

use reddb::replication::cdc::ChangeRecord;
use reddb::replication::logical::{
    ApplyMode, ApplyOutcome, LogicalApplyError, LogicalChangeApplier,
};
use reddb::storage::RedDB;
use std::path::PathBuf;

#[allow(dead_code)]
mod support;

fn temp_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-chaos-{prefix}-{}-{}.rdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn record(lsn: u64, payload: &[u8]) -> ChangeRecord {
    support::logical_insert_record("users", lsn, 1000 + lsn, payload)
}

#[test]
fn replica_applier_fails_closed_on_lsn_collision_diff_payload() {
    let path = temp_path("divergence");
    let _ = std::fs::remove_file(&path);
    let db = RedDB::open(&path).unwrap();
    let applier = LogicalChangeApplier::new(0);

    assert_eq!(
        applier
            .apply(&db, &record(7, b"original"), ApplyMode::Replica)
            .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(applier.last_applied_lsn(), 7);

    // Same LSN, different payload — corruption / split-brain signal.
    let err = applier
        .apply(&db, &record(7, b"forged"), ApplyMode::Replica)
        .expect_err("divergence must fail closed");
    match err {
        LogicalApplyError::Divergence { lsn, expected, got } => {
            assert_eq!(lsn, 7);
            assert_ne!(expected, got, "hashes must differ");
        }
        other => panic!("expected Divergence, got {other:?}"),
    }

    // Idempotent path: same payload at same LSN must skip cleanly.
    assert_eq!(
        applier
            .apply(&db, &record(7, b"original"), ApplyMode::Replica)
            .unwrap(),
        ApplyOutcome::Idempotent
    );

    let _ = std::fs::remove_file(&path);
}
