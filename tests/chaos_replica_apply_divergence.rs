//! Chaos test: replica LSN-collision divergence fail-closed
//! (PLAN.md Phase 11.5 + 8 slice).
//!
//! Same LSN, different payload bytes is the strongest signal a replica
//! has of primary corruption or split-brain. Apply must fail closed.

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
fn replica_applier_fails_closed_on_lsn_collision_diff_payload() {
    let path = temp_path("divergence");
    let _ = std::fs::remove_file(&path);
    let db = RedDB::open(&path).unwrap();
    let applier = LogicalChangeApplier::new(0);

    assert_eq!(
        applier.apply(&db, &record(7, b"original"), ApplyMode::Replica).unwrap(),
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
        applier.apply(&db, &record(7, b"original"), ApplyMode::Replica).unwrap(),
        ApplyOutcome::Idempotent
    );

    let _ = std::fs::remove_file(&path);
}
