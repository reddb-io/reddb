//! Issue #835 — fence a stale-term ex-primary after a failover
//! (PRD #819, ADR 0030).
//!
//! End-to-end: induce a real term handover (term 5 → 6 via the
//! [`FailoverCoordinator`]), then bring the *old* primary back on its stale
//! term and prove it is fenced at every boundary it could re-enter through:
//!
//! 1. **Apply boundary** — a replica that has followed the new primary to
//!    term 6 rejects a record stamped with the stale term 5
//!    ([`LogicalChangeApplier`]), failing closed without advancing its
//!    watermark; and
//! 2. **Stream-handshake boundary** — a replica refuses a replication-stream
//!    handshake announcing the stale term ([`TermFence`]); and
//! 3. **Writer lease** — the lease generation is tied to the term, so the
//!    stale ex-primary can neither poach the lease nor keep mutating under a
//!    lease taken on the old term ([`LeaseStore`] / [`WriterLease`]).
//!
//! Finally it proves the gate opens again the moment the ex-primary re-syncs
//! and adopts the new term — fencing keeps the timeline safe, it does not
//! ban a recovered node forever.

use std::sync::Arc;
use std::time::Duration;

use reddb::api::REDDB_FORMAT_VERSION;
use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
use reddb::replication::failover::{
    FailoverCoordinator, FailoverMode, FailoverNode, FailoverRequest, FailoverTransport,
};
use reddb::replication::fence::{
    FenceBoundary, FenceVerdict, MemoryTermStore, StreamHandshake, TermFence,
};
use reddb::replication::lease::{LeaseError, LeaseStore, WriterLease};
use reddb::replication::logical::{
    ApplyMode, ApplyOutcome, LogicalApplyError, LogicalChangeApplier,
};
use reddb::storage::schema::Value;
use reddb::storage::{EntityData, EntityId, EntityKind, RedDB, RowData, UnifiedEntity};

// ---------------------------------------------------------------------------
// Step 1 — induce a real failover that advances the term 5 -> 6.
// ---------------------------------------------------------------------------

/// A minimal scripted transport whose target is already caught up, so the
/// handover takes the fast path and mints `current_term + 1`.
struct CaughtUpCluster {
    frontier: u64,
    committed_term: Option<u64>,
}

impl FailoverTransport for CaughtUpCluster {
    fn freeze_primary(&mut self) -> u64 {
        self.frontier
    }
    fn resume_primary(&mut self) {}
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
    fn poll_target_frontier(&mut self) -> u64 {
        self.frontier
    }
    fn commit_handover(&mut self, new_term: u64) {
        self.committed_term = Some(new_term);
    }
}

fn induce_failover_to_new_term(current_term: u64, frontier: u64) -> u64 {
    let mut cluster = CaughtUpCluster {
        frontier,
        committed_term: None,
    };
    let req = FailoverRequest {
        old_primary: FailoverNode::new("old", "http://old:50051", "us-east"),
        target: FailoverNode::new("new", "http://new:50051", "us-west"),
        current_term,
        target_frontier_hint: frontier, // already caught up
        mode: FailoverMode::Coordinated {
            catch_up_deadline: Duration::from_secs(1),
        },
    };
    let outcome = FailoverCoordinator::run(&req, &mut cluster).expect("clean handover");
    assert_eq!(cluster.committed_term, Some(current_term + 1));
    outcome.new_term
}

// ---------------------------------------------------------------------------
// A real change record helper (mirrors the apply-path unit tests).
// ---------------------------------------------------------------------------

fn change_record(lsn: u64, term: u64, payload: &[u8]) -> ChangeRecord {
    let timestamp = 100 + lsn;
    let mut entity = UnifiedEntity::new(
        EntityId::new(lsn),
        EntityKind::TableRow {
            table: std::sync::Arc::from("accounts"),
            row_id: lsn,
        },
        EntityData::Row(RowData::with_names(
            vec![Value::UnsignedInteger(lsn), Value::Blob(payload.to_vec())],
            vec!["id".to_string(), "payload".to_string()],
        )),
    );
    entity.created_at = timestamp;
    entity.updated_at = timestamp;
    entity.sequence_id = lsn;
    ChangeRecord::from_entity(
        lsn,
        timestamp,
        ChangeOperation::Insert,
        "accounts",
        "row",
        &entity,
        REDDB_FORMAT_VERSION,
        None,
    )
    .with_term(term)
}

fn open_replica_db(tag: &str) -> (RedDB, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "reddb_issue835_{tag}_{}_{}",
        std::process::id(),
        reddb::utils::now_unix_nanos(),
    ));
    let _ = std::fs::remove_file(&path);
    let db = RedDB::open(&path).expect("open replica db");
    (db, path)
}

fn term_fence(term: u64) -> TermFence<MemoryTermStore> {
    TermFence::new(MemoryTermStore::seeded(term))
}

// ---------------------------------------------------------------------------
// Acceptance: replicas reject records carrying a stale term at the apply
// boundary, and a returning ex-primary cannot advance a watermark until it
// adopts the new term.
// ---------------------------------------------------------------------------

#[test]
fn stale_term_record_is_fenced_at_the_apply_boundary() {
    let new_term = induce_failover_to_new_term(5, 42);
    assert_eq!(new_term, 6);

    let (db, path) = open_replica_db("apply");
    let applier = LogicalChangeApplier::new(0);

    // The replica follows the new primary to (term 6, lsn 2).
    assert_eq!(
        applier
            .apply(&db, &change_record(1, new_term, b"a"), ApplyMode::Replica)
            .unwrap(),
        ApplyOutcome::Applied
    );
    applier
        .apply(&db, &change_record(2, new_term, b"b"), ApplyMode::Replica)
        .unwrap();
    let watermark_before = applier.last_applied_lsn();
    assert_eq!(watermark_before, 2);

    // The deposed ex-primary returns and streams the next LSN under the
    // stale term 5. The apply boundary fences it.
    let stale = change_record(3, 5, b"c");
    let err = applier
        .apply(&db, &stale, ApplyMode::Replica)
        .expect_err("stale-term record must be fenced");
    assert!(
        matches!(
            err,
            LogicalApplyError::StaleTermFenced {
                current_term: 6,
                record_term: 5,
                lsn: 3,
            }
        ),
        "got {err:?}"
    );

    // Fail closed: the watermark did not move — the ex-primary advanced
    // nothing on the replica.
    assert_eq!(
        applier.last_applied_lsn(),
        watermark_before,
        "a fenced stale-term record must not advance the watermark"
    );
    assert_eq!(applier.last_applied_term(), 6, "term must not regress");

    // The ex-primary re-syncs and adopts the new term; the same LSN now
    // applies and the watermark advances.
    applier
        .apply(&db, &change_record(3, new_term, b"c"), ApplyMode::Replica)
        .expect("a record under the adopted term applies");
    assert_eq!(applier.last_applied_lsn(), 3);

    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// Acceptance: replicas reject stream handshakes carrying a stale term.
// ---------------------------------------------------------------------------

#[test]
fn stale_term_handshake_is_refused_at_the_handshake_boundary() {
    let new_term = induce_failover_to_new_term(5, 42);
    let fence = term_fence(new_term);

    // The returning ex-primary opens a stream announcing its stale term 5.
    let stale = StreamHandshake::new("old-primary", 5);
    let rejection = match fence
        .admit_stream_handshake(&stale)
        .expect("handshake classification should read the term store")
    {
        FenceVerdict::Fenced(rejection) => rejection,
        other => panic!("expected stale handshake to be fenced, got {other:?}"),
    };
    assert_eq!(rejection.boundary, FenceBoundary::Handshake);
    assert_eq!(rejection.incoming_term, 5);
    assert_eq!(rejection.current_term, 6);

    // A handshake at the current term is admitted.
    let current = StreamHandshake::new("new-primary", new_term);
    assert!(
        fence
            .admit_stream_handshake(&current)
            .expect("handshake classification should read the term store")
            .is_admitted(),
        "a current-term handshake must be admitted",
    );
}

// ---------------------------------------------------------------------------
// Acceptance: the writer lease generation is tied to the term so a stale
// holder fails closed.
// ---------------------------------------------------------------------------

fn lease_store(tag: &str) -> LeaseStore {
    use reddb::storage::backend::LocalBackend;
    LeaseStore::new(Arc::new(LocalBackend)).with_prefix(format!(
        "{}/reddb-issue835-lease-{tag}-{}",
        std::env::temp_dir().to_string_lossy(),
        reddb::utils::now_unix_nanos(),
    ))
}

#[test]
fn stale_term_lease_holder_fails_closed() {
    let new_term = induce_failover_to_new_term(5, 42);
    let store = lease_store("lease");

    // The newly elected primary takes the writer lease under term 6 with a
    // tiny TTL, then it lapses — so the only thing standing between the stale
    // ex-primary and the (now poachable) lease is the term fence, not the TTL.
    let promoted = store
        .try_acquire_for_term("main", "new-primary", 1, new_term)
        .expect("new primary acquires the lease");
    assert_eq!(promoted.term, new_term);
    std::thread::sleep(Duration::from_millis(10));

    // The returning ex-primary, on stale term 5, cannot poach the lease even
    // though it has expired by TTL — the term fence holds regardless.
    let err = store
        .try_acquire_for_term("main", "old-primary", 60_000, 5)
        .expect_err("stale-term contender must fail closed");
    assert!(
        matches!(
            err,
            LeaseError::Fenced {
                current_term: 6,
                ..
            }
        ),
        "got {err:?}"
    );

    // And a holder whose lease was taken under the old term fails closed at
    // mutate time once the cluster term advanced.
    let stale_lease = WriterLease {
        database_key: "main".to_string(),
        holder_id: "old-primary".to_string(),
        generation: 1,
        term: 5,
        acquired_at_ms: 0,
        expires_at_ms: u64::MAX, // TTL still valid — only the term fences it
    };
    assert!(
        stale_lease.fenced_by_term(new_term),
        "a lease on the old term must fail closed at mutate time"
    );

    // Once the ex-primary re-syncs and re-acquires under the new term, the
    // lease admits it again (generation advances with the term).
    let resynced = store
        .try_acquire_for_term("main", "old-primary", 1, new_term)
        .expect("re-syncing under the new term may re-take an expired lease");
    assert_eq!(resynced.term, new_term);
}

// ---------------------------------------------------------------------------
// One scenario tying it together: a single stale ex-primary is fenced at all
// three boundaries at once, then admitted everywhere after it adopts the new
// term. This is the issue #835 contract in one place.
// ---------------------------------------------------------------------------

#[test]
fn returning_ex_primary_is_fenced_everywhere_then_admitted_after_resync() {
    let new_term = induce_failover_to_new_term(5, 100);
    let stale_term = 5;

    // Boundaries, each anchored at the current term.
    let fence = term_fence(new_term);
    let (db, path) = open_replica_db("all");
    let applier = LogicalChangeApplier::new(0);
    applier
        .apply(&db, &change_record(1, new_term, b"x"), ApplyMode::Replica)
        .unwrap();
    let store = lease_store("all");
    let _new_primary_lease = store
        .try_acquire_for_term("main", "new-primary", 60_000, new_term)
        .unwrap();

    // --- stale ex-primary fenced at every boundary ---
    assert_eq!(
        match fence
            .admit_stream_handshake(&StreamHandshake::new("old", stale_term))
            .unwrap()
        {
            FenceVerdict::Fenced(rejection) => rejection.boundary,
            other => panic!("expected stale handshake to be fenced, got {other:?}"),
        },
        FenceBoundary::Handshake
    );
    assert!(
        applier
            .apply(&db, &change_record(2, stale_term, b"y"), ApplyMode::Replica)
            .is_err(),
        "stale record fenced at apply boundary"
    );
    assert!(
        store
            .try_acquire_for_term("main", "old", 60_000, stale_term)
            .is_err(),
        "stale contender fenced at lease boundary"
    );
    // Nothing advanced: the replica's watermark is still at lsn 1.
    assert_eq!(applier.last_applied_lsn(), 1);

    // --- ex-primary re-syncs, adopts the new term, and is admitted ---
    let adopting = term_fence(stale_term);
    adopting.adopt(new_term).expect("adopts the newer term");
    assert_eq!(adopting.current_term().unwrap(), new_term);
    assert!(fence
        .admit_stream_handshake(&StreamHandshake::new("old", new_term))
        .unwrap()
        .is_admitted());
    applier
        .apply(&db, &change_record(2, new_term, b"y"), ApplyMode::Replica)
        .expect("record under the adopted term applies");
    assert_eq!(applier.last_applied_lsn(), 2, "watermark advances again");

    let _ = std::fs::remove_file(path);
}
