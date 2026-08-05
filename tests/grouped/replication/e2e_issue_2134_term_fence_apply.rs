//! Issue #2134 — the live logical apply path uses `TermFence`.

use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
use reddb::replication::logical::{ApplyMode, LogicalApplyError, LogicalChangeApplier};
use reddb::{RedDBOptions, RedDBRuntime};

fn record(term: u64, operation: ChangeOperation) -> ChangeRecord {
    ChangeRecord {
        term,
        lsn: 1,
        timestamp: 1,
        operation,
        collection: "term_fence_items".to_string(),
        entity_id: 1,
        entity_kind: "row".to_string(),
        entity_bytes: None,
        metadata: None,
        refresh_records: None,
        range_id: None,
        ownership_epoch: None,
    }
}

#[test]
fn failed_new_term_record_still_fences_stale_term_in_live_apply_loop() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("in-memory replica runtime boots");
    let applier = LogicalChangeApplier::new(0);

    let invalid_new_term_record = record(6, ChangeOperation::Insert);
    assert!(
        matches!(
            applier.apply_fenced(
                runtime.db().as_ref(),
                &invalid_new_term_record,
                ApplyMode::Replica,
                None,
            ),
            Err(LogicalApplyError::Apply { lsn: 1, .. })
        ),
        "the missing entity payload must fail after the term fence admits term 6"
    );
    assert_eq!(applier.last_applied_lsn(), 0);

    let stale_record = record(5, ChangeOperation::Delete);
    let err = applier
        .apply_fenced(
            runtime.db().as_ref(),
            &stale_record,
            ApplyMode::Replica,
            None,
        )
        .expect_err("TermFence must reject a record behind its adopted term");

    assert!(
        matches!(
            err,
            LogicalApplyError::StaleTermFenced {
                record_term: 5,
                current_term: 6,
                lsn: 1,
            }
        ),
        "got {err:?}"
    );
    assert_eq!(
        applier.last_applied_lsn(),
        0,
        "stale apply must fail closed"
    );
}
