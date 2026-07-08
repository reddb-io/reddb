//! Issue #1843 — replica apply rejects deposed-owner authority divergence.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use reddb::replication::cdc::{ChangeOperation, RangeAdmitError};
use reddb::replication::logical::{
    ApplyErrorKind, ApplyMode, LogicalApplyError, LogicalChangeApplier, ReplicaApplyMetrics,
};
use reddb::{RedDBOptions, RedDBRuntime, ReplicationConfig};
use reddb_wire::replication::ChangeRecord;

#[test]
fn deposed_owner_stream_against_promoted_group_is_rejected() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory()
            .with_replication(ReplicationConfig::replica("http://primary:55055").with_term(8)),
    )
    .expect("replica runtime boots at promoted term");
    let authority = runtime
        .write_gate_arc()
        .primary_replica_range_authority()
        .expect("replica has apply authority");
    let applier = LogicalChangeApplier::with_metrics(0, Arc::new(ReplicaApplyMetrics::default()));

    let record = ChangeRecord {
        term: 7,
        lsn: 1,
        timestamp: 1,
        operation: ChangeOperation::Delete,
        collection: "authority_items".to_string(),
        entity_id: 1,
        entity_kind: "row".to_string(),
        entity_bytes: None,
        metadata: None,
        refresh_records: None,
        range_id: Some(authority.range_id),
        ownership_epoch: Some(authority.min_ownership_epoch),
    };

    let err = applier
        .apply_fenced(
            runtime.db().as_ref(),
            &record,
            ApplyMode::Replica,
            Some(&authority),
        )
        .expect_err("deposed owner stream must be rejected");

    assert!(
        matches!(
            err,
            LogicalApplyError::RangeFenced {
                range_id,
                lsn: 1,
                reason: RangeAdmitError::StaleTerm {
                    record_term: 7,
                    accepted_term: 8,
                },
            } if range_id == authority.range_id
        ),
        "got {err:?}"
    );
    assert_eq!(err.kind(), ApplyErrorKind::Fenced);
    assert_eq!(
        applier.last_applied_lsn(),
        0,
        "apply must halt before LSN advance"
    );
    assert_eq!(
        applier.metrics().fenced_total.load(Ordering::Relaxed),
        1,
        "the refusal must leave a metrics trail"
    );
}
