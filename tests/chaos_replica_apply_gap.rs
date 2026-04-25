//! Chaos test: replica LSN gap fail-closed (PLAN.md Phase 11.5 + 8 slice).
//!
//! Drives a `LogicalChangeApplier` end-to-end with records that skip an
//! LSN. The first record is accepted (anchors the chain); the second
//! must return `LogicalApplyError::Gap` so the replica fetcher marks
//! the instance unhealthy instead of silently advancing past missing
//! records.

use reddb::api::REDDB_FORMAT_VERSION;
use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
use reddb::replication::logical::{ApplyMode, ApplyOutcome, LogicalApplyError, LogicalChangeApplier};
use reddb::storage::{EntityId, RedDB, UnifiedEntity, UnifiedStore};
use std::path::PathBuf;

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
    let entity = UnifiedEntity::new(EntityId::new(lsn), payload.to_vec());
    ChangeRecord {
        lsn,
        timestamp: 1000 + lsn,
        operation: ChangeOperation::Insert,
        collection: "users".to_string(),
        entity_id: lsn,
        entity_kind: "row".to_string(),
        entity_bytes: Some(UnifiedStore::serialize_entity(&entity, REDDB_FORMAT_VERSION)),
        metadata: None,
    }
}

#[test]
fn replica_applier_returns_gap_when_record_skips_lsn() {
    let path = temp_path("apply-gap");
    let _ = std::fs::remove_file(&path);
    let db = RedDB::open(&path).unwrap();
    let applier = LogicalChangeApplier::new(0);

    // Anchor the chain at LSN 1.
    assert_eq!(
        applier.apply(&db, &record(1, b"a"), ApplyMode::Replica).unwrap(),
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
