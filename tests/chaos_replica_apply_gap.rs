//! Chaos test: replica LSN gap fail-closed (PLAN.md Phase 11.5 + 8 slice).
//!
//! Drives a `LogicalChangeApplier` end-to-end with records that skip an
//! LSN. The first record is accepted (anchors the chain); the second
//! must return `LogicalApplyError::Gap` so the replica fetcher marks
//! the instance unhealthy instead of silently advancing past missing
//! records.

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
fn replica_applier_returns_gap_when_record_skips_lsn() {
    let path = temp_path("apply-gap");
    let _ = std::fs::remove_file(&path);
    let db = RedDB::open(&path).unwrap();
    let applier = LogicalChangeApplier::new(0);

    // Anchor the chain at LSN 1.
    assert_eq!(
        applier
            .apply(&db, &record(1, b"a"), ApplyMode::Replica)
            .unwrap(),
        ApplyOutcome::Applied,
    );
    assert_eq!(applier.last_applied_lsn(), 1);

    // Skip LSN 2 — fetcher hands LSN 5 directly. Must fail closed.
    let err = applier
        .apply(&db, &record(5, b"e"), ApplyMode::Replica)
        .expect_err("gap must fail closed");
    match err {
        LogicalApplyError::Gap { last, next } => {
            assert_eq!(last, 1);
            assert_eq!(next, 5);
        }
        other => panic!("expected Gap, got {other:?}"),
    }

    // Critical invariant: state must NOT advance on Gap.
    assert_eq!(
        applier.last_applied_lsn(),
        1,
        "applier must stay at 1 after Gap; advancing would silently swallow corruption"
    );

    let _ = std::fs::remove_file(&path);
}
