//! Logical replication helpers shared by replica apply and point-in-time restore.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use crate::api::{RedDBError, RedDBResult};
use crate::application::entity::metadata_from_json;
use crate::replication::cdc::{
    change_record_from_entity, wire_json_to_server_json, ChangeOperation, ChangeRecord,
    RangeAdmitError, RangeAuthority,
};
use crate::storage::{EntityId, EntityKind, RedDB, UnifiedStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Replica,
    Restore,
}

/// PLAN.md Phase 11.5 — counters the replica apply loop bumps when an
/// invariant breaks. Surfaced via `reddb_replica_apply_errors_total`.
/// Decode errors aren't strictly apply errors but they share the same
/// observability lane so dashboards alert on "replica is ingesting
/// trash from primary regardless of cause".
#[derive(Debug, Default)]
pub struct ReplicaApplyMetrics {
    pub gap_total: std::sync::atomic::AtomicU64,
    pub divergence_total: std::sync::atomic::AtomicU64,
    pub apply_error_total: std::sync::atomic::AtomicU64,
    pub decode_error_total: std::sync::atomic::AtomicU64,
    /// Issue #814 — a delete (or other apply) that found no target on the
    /// replica: a missing collection or a missing entity. Non-fatal (the
    /// LSN chain still advances so idempotent re-pull converges, see
    /// #813), but recorded so a missed delete that drives collection-count
    /// drift leaves a trail instead of being swallowed by `let _ =`.
    pub apply_miss_total: std::sync::atomic::AtomicU64,
    /// Issue #835 — a record carrying a term *behind* the replica's current
    /// term was fenced at the apply boundary (a returning ex-primary on a
    /// stale term). Fail-closed: the record is rejected and the LSN/term
    /// chain does not advance, so the deposed primary cannot move any
    /// watermark until it re-syncs under the new term.
    pub fenced_total: std::sync::atomic::AtomicU64,
    /// Issue #1242 — entity-payload bytes from successfully applied WAL
    /// records (entity_bytes for insert/update, refresh payload for
    /// refresh; deletes carry no payload and contribute 0). Monotonic
    /// within one process lifetime; reset-aware for readers after restart.
    /// Surfaced via `reddb_replication_apply_bytes_total`.
    pub bytes_applied_total: std::sync::atomic::AtomicU64,
    /// Issue #1242 — count of WAL records successfully applied (Applied
    /// outcome only; Idempotent and Skipped are excluded). Monotonic
    /// within one process lifetime. Surfaced via
    /// `reddb_replication_apply_records_total`.
    pub records_applied_total: std::sync::atomic::AtomicU64,
}

impl ReplicaApplyMetrics {
    pub fn record(&self, kind: ApplyErrorKind) {
        use std::sync::atomic::Ordering::Relaxed;
        match kind {
            ApplyErrorKind::Gap => {
                self.gap_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Divergence => {
                self.divergence_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Apply => {
                self.apply_error_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Decode => {
                self.decode_error_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Miss => {
                self.apply_miss_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Fenced => {
                self.fenced_total.fetch_add(1, Relaxed);
            }
        }
    }

    pub fn snapshot(&self) -> [(ApplyErrorKind, u64); 6] {
        use std::sync::atomic::Ordering::Relaxed;
        [
            (ApplyErrorKind::Gap, self.gap_total.load(Relaxed)),
            (
                ApplyErrorKind::Divergence,
                self.divergence_total.load(Relaxed),
            ),
            (ApplyErrorKind::Apply, self.apply_error_total.load(Relaxed)),
            (
                ApplyErrorKind::Decode,
                self.decode_error_total.load(Relaxed),
            ),
            (ApplyErrorKind::Miss, self.apply_miss_total.load(Relaxed)),
            (ApplyErrorKind::Fenced, self.fenced_total.load(Relaxed)),
        ]
    }

    /// Issue #1242 — throughput snapshot: `(bytes_applied_total,
    /// records_applied_total)`. Both are monotonic within a process
    /// lifetime and zero after restart, so callers that track rate must
    /// detect a reset (new value < prior value) and treat it as a restart.
    pub fn snapshot_throughput(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.bytes_applied_total.load(Relaxed),
            self.records_applied_total.load(Relaxed),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyErrorKind {
    Gap,
    Divergence,
    Apply,
    Decode,
    /// Issue #814 — apply ran against a missing target (delete on an
    /// absent collection/entity). Non-fatal divergence signal.
    Miss,
    /// Issue #835 — a record from a term behind the replica's current term
    /// was fenced at the apply boundary (a stale ex-primary). Fail-closed.
    Fenced,
}

impl ApplyErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Divergence => "divergence",
            Self::Apply => "apply",
            Self::Decode => "decode",
            Self::Miss => "apply_miss",
            Self::Fenced => "fenced",
        }
    }
}

impl LogicalApplyError {
    pub fn kind(&self) -> ApplyErrorKind {
        match self {
            Self::Gap { .. } => ApplyErrorKind::Gap,
            Self::Divergence { .. } => ApplyErrorKind::Divergence,
            Self::Apply { .. } => ApplyErrorKind::Apply,
            Self::StaleTermFenced { .. } => ApplyErrorKind::Fenced,
            Self::RangeFenced { .. } => ApplyErrorKind::Fenced,
        }
    }
}

/// Outcome of a single `apply` call. `Applied` advances the chain;
/// `Idempotent` and `Skipped` are no-ops (we already saw an
/// equal-or-newer LSN). `Gap` and `Divergence` (returned via
/// `LogicalApplyError`) are fail-closed — callers (replica fetcher,
/// restore loop) should mark the instance unhealthy and stop applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Normal monotonic advance.
    Applied,
    /// Same LSN as last applied with same payload hash — log + skip.
    Idempotent,
    /// Older LSN than what we already have — log + skip.
    Skipped,
}

#[derive(Debug)]
pub enum LogicalApplyError {
    Gap {
        last: u64,
        next: u64,
    },
    Divergence {
        expected_term: u64,
        got_term: u64,
        lsn: u64,
        expected: String,
        got: String,
    },
    Apply {
        lsn: u64,
        source: RedDBError,
    },
    /// Issue #835 — the record's term is behind the replica's current
    /// term: a returning ex-primary streaming on a stale, superseded term.
    /// Rejected at the apply boundary so the deposed primary cannot apply
    /// or advance any watermark until it re-syncs under the new term.
    StaleTermFenced {
        record_term: u64,
        current_term: u64,
        lsn: u64,
    },
    /// Issue #991 — the record is stamped for a range whose authority
    /// watermark has moved past it: a write from a deposed range owner
    /// (stale ownership epoch) or a superseded timeline (stale term) for the
    /// target range. Rejected at the apply boundary before the LSN state
    /// machine runs — fail-closed, so a stale owner cannot apply or advance
    /// the chain/watermark for that range. Shares the `Fenced` metrics lane
    /// with the global stale-term fence.
    RangeFenced {
        range_id: u64,
        lsn: u64,
        reason: RangeAdmitError,
    },
}

impl std::fmt::Display for LogicalApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gap { last, next } => write!(f, "LSN gap on apply: last={last} next={next}"),
            Self::StaleTermFenced {
                record_term,
                current_term,
                lsn,
            } => write!(
                f,
                "stale-term record fenced at lsn={lsn}: record term {record_term} is behind current term {current_term}"
            ),
            Self::RangeFenced {
                range_id,
                lsn,
                reason,
            } => match reason {
                RangeAdmitError::StaleTerm {
                    record_term,
                    accepted_term,
                } => write!(
                    f,
                    "range-stale record fenced at lsn={lsn} for range {range_id}: record term {record_term} is behind accepted term {accepted_term}"
                ),
                RangeAdmitError::StaleOwnershipEpoch {
                    record_epoch,
                    accepted_epoch,
                } => write!(
                    f,
                    "range-stale record fenced at lsn={lsn} for range {range_id}: ownership epoch {record_epoch} is behind accepted epoch {accepted_epoch}"
                ),
            },
            Self::Divergence {
                expected_term,
                got_term,
                lsn,
                expected,
                got,
            } => write!(
                f,
                "LSN divergence on apply at term/lsn=({got_term},{lsn}): expected term {expected_term} payload hash {expected}, got {got}"
            ),
            Self::Apply { lsn, source } => write!(f, "apply error at lsn={lsn}: {source}"),
        }
    }
}

impl std::error::Error for LogicalApplyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkWaitError {
    Timeout { target_lsn: u64, applied_lsn: u64 },
    TermMismatch { target_term: u64, applied_term: u64 },
}

impl BookmarkWaitError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout { .. })
    }
}

impl std::fmt::Display for BookmarkWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout {
                target_lsn,
                applied_lsn,
            } => write!(
                f,
                "timed out waiting for causal bookmark lsn {target_lsn}; applied={applied_lsn}"
            ),
            Self::TermMismatch {
                target_term,
                applied_term,
            } => write!(
                f,
                "causal bookmark term mismatch: target={target_term} applied={applied_term}"
            ),
        }
    }
}

impl std::error::Error for BookmarkWaitError {}

/// Shared logical change applier so replica replay and PITR converge on the
/// same semantics. Stateful (PLAN.md Phase 11.5): tracks the last applied
/// LSN + payload hash so duplicates / older LSNs / gaps / divergences are
/// detected explicitly.
pub struct LogicalChangeApplier {
    last_applied_term: AtomicU64,
    last_applied_lsn: AtomicU64,
    received_frontier_lsn: AtomicU64,
    last_payload_hash: Mutex<Option<[u8; 32]>>,
    apply_wait: (Mutex<()>, Condvar),
    /// Issue #814 — metrics the apply path bumps when a record runs
    /// against a missing target. The production replica loop shares the
    /// runtime's `ReplicaApplyMetrics` here so `/metrics` surfaces misses;
    /// other callers (PITR, tests) get a private default that no one reads.
    metrics: std::sync::Arc<ReplicaApplyMetrics>,
}

impl LogicalChangeApplier {
    /// Build a fresh applier. `starting_lsn` is the LSN already
    /// covered by the snapshot (or `0` for an empty replica). The
    /// next acceptable record is any positive LSN; from there the
    /// chain advances by 1.
    pub fn new(starting_lsn: u64) -> Self {
        Self::with_metrics(
            starting_lsn,
            std::sync::Arc::new(ReplicaApplyMetrics::default()),
        )
    }

    /// Build an applier that records apply misses / errors into a shared
    /// `ReplicaApplyMetrics` (issue #814). The production replica loop
    /// passes the runtime's metrics so a swallowed delete leaves a trail
    /// on `reddb_replica_apply_errors_total{kind="apply_miss"}`.
    pub fn with_metrics(starting_lsn: u64, metrics: std::sync::Arc<ReplicaApplyMetrics>) -> Self {
        Self {
            last_applied_term: AtomicU64::new(crate::replication::DEFAULT_REPLICATION_TERM),
            last_applied_lsn: AtomicU64::new(starting_lsn),
            received_frontier_lsn: AtomicU64::new(starting_lsn),
            last_payload_hash: Mutex::new(None),
            apply_wait: (Mutex::new(()), Condvar::new()),
            metrics,
        }
    }

    /// The metrics handle this applier records misses/errors into.
    pub fn metrics(&self) -> &std::sync::Arc<ReplicaApplyMetrics> {
        &self.metrics
    }

    pub fn last_applied_lsn(&self) -> u64 {
        self.last_applied_lsn.load(Ordering::Acquire)
    }

    pub fn received_frontier_lsn(&self) -> u64 {
        self.received_frontier_lsn.load(Ordering::Acquire)
    }

    pub fn last_applied_term(&self) -> u64 {
        self.last_applied_term.load(Ordering::Acquire)
    }

    pub fn wait_for_bookmark(
        &self,
        bookmark: &crate::replication::CausalBookmark,
        timeout: std::time::Duration,
    ) -> Result<(), BookmarkWaitError> {
        let deadline = std::time::Instant::now() + timeout;
        let target_lsn = bookmark.commit_lsn();
        let target_term = bookmark.term();

        let mut guard = self.apply_wait.0.lock().expect("apply wait mutex");
        loop {
            let applied_lsn = self.last_applied_lsn();
            let applied_term = self.last_applied_term();
            if applied_lsn >= target_lsn {
                if applied_term == target_term {
                    return Ok(());
                }
                return Err(BookmarkWaitError::TermMismatch {
                    target_term,
                    applied_term,
                });
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(BookmarkWaitError::Timeout {
                    target_lsn,
                    applied_lsn,
                });
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next_guard, wait_result) = self
                .apply_wait
                .1
                .wait_timeout(guard, remaining)
                .expect("apply wait condvar");
            guard = next_guard;
            if wait_result.timed_out() {
                return Err(BookmarkWaitError::Timeout {
                    target_lsn,
                    applied_lsn: self.last_applied_lsn(),
                });
            }
        }
    }

    /// Apply one logical change record. The state machine:
    /// - first record after `starting_lsn == 0` → apply, anchor.
    /// - `lsn == last + 1` → apply, advance.
    /// - `lsn == last` && payload hash equal → idempotent skip.
    /// - `lsn == last` && payload hash differs → `Divergence` (fail closed).
    /// - `lsn < last` → older replay, skip with debug log.
    /// - `lsn > last + 1` → `Gap` (fail closed; caller marks unhealthy).
    pub fn apply(
        &self,
        db: &RedDB,
        record: &ChangeRecord,
        mode: ApplyMode,
    ) -> Result<ApplyOutcome, LogicalApplyError> {
        self.apply_fenced(db, record, mode, None)
    }

    /// Apply one record, first gating it against the target range's authority
    /// watermark (issue #991). When `range_fence` is `Some`, a record stamped
    /// for that range whose term or ownership epoch is behind the watermark is
    /// rejected before the global term fence and the LSN state machine run —
    /// fail-closed, so a deposed range owner cannot advance anything. Records
    /// for a different range, or with no range identity (legacy /
    /// non-range-replicated), pass the range fence untouched. `apply` is the
    /// unfenced shorthand for callers that do not yet hold range authority.
    pub fn apply_fenced(
        &self,
        db: &RedDB,
        record: &ChangeRecord,
        mode: ApplyMode,
        range_fence: Option<&RangeAuthority>,
    ) -> Result<ApplyOutcome, LogicalApplyError> {
        let last = self.last_applied_lsn.load(Ordering::Acquire);
        let last_term = self.last_applied_term.load(Ordering::Acquire);

        // Per-range authority fence (issue #991). Runs before the global
        // term fence so a stale ownership epoch is rejected even when the
        // record's term is otherwise current. Only records stamped for the
        // fence's range are gated; the rest fall through.
        if let Some(fence) = range_fence {
            if let Err(reason) = fence.admit(record) {
                self.metrics.record(ApplyErrorKind::Fenced);
                return Err(LogicalApplyError::RangeFenced {
                    range_id: fence.range_id,
                    lsn: record.lsn,
                    reason,
                });
            }
        }

        // Stale-term fence (issue #835, ADR 0030). A record from a term
        // *behind* the highest term this replica has adopted is a returning
        // ex-primary on a superseded timeline. Reject it before the LSN
        // state machine runs — fail closed regardless of LSN, so a stale
        // ex-primary can neither apply nor advance the chain/watermark. A
        // record on the *same* term is admitted; a *higher* term is the new
        // primary's timeline and is adopted on apply below. This mirrors the
        // election-side `RefusalReason::StaleTerm` on the data path.
        if record.term < last_term {
            self.metrics.record(ApplyErrorKind::Fenced);
            return Err(LogicalApplyError::StaleTermFenced {
                record_term: record.term,
                current_term: last_term,
                lsn: record.lsn,
            });
        }

        let payload_hash = record_payload_hash(record);
        self.received_frontier_lsn
            .fetch_max(record.lsn, Ordering::AcqRel);

        if last == 0 && record.lsn > 0 {
            self.do_apply(db, record, mode)?;
            self.last_applied_term.store(record.term, Ordering::Release);
            self.last_applied_lsn.store(record.lsn, Ordering::Release);
            *self.last_payload_hash.lock().expect("payload hash mutex") = Some(payload_hash);
            self.apply_wait.1.notify_all();
            return Ok(ApplyOutcome::Applied);
        }

        if record.lsn == last {
            let prior = *self.last_payload_hash.lock().expect("payload hash mutex");
            return match prior {
                Some(p) if p == payload_hash => Ok(ApplyOutcome::Idempotent),
                Some(p) => Err(LogicalApplyError::Divergence {
                    expected_term: last_term,
                    got_term: record.term,
                    lsn: record.lsn,
                    expected: hex_digest(&p),
                    got: hex_digest(&payload_hash),
                }),
                None => Ok(ApplyOutcome::Idempotent),
            };
        }
        if record.lsn < last {
            return Ok(ApplyOutcome::Skipped);
        }
        if record.lsn > last + 1 {
            return Err(LogicalApplyError::Gap {
                last,
                next: record.lsn,
            });
        }

        self.do_apply(db, record, mode)?;
        self.last_applied_term.store(record.term, Ordering::Release);
        self.last_applied_lsn.store(record.lsn, Ordering::Release);
        *self.last_payload_hash.lock().expect("payload hash mutex") = Some(payload_hash);
        self.apply_wait.1.notify_all();
        Ok(ApplyOutcome::Applied)
    }

    fn do_apply(
        &self,
        db: &RedDB,
        record: &ChangeRecord,
        mode: ApplyMode,
    ) -> Result<(), LogicalApplyError> {
        Self::apply_record_with_metrics(db, record, mode, &self.metrics).map_err(|err| {
            LogicalApplyError::Apply {
                lsn: record.lsn,
                source: err,
            }
        })
    }

    /// Stateless apply — applies the record without monotonicity
    /// checks. Kept for callers that don't yet thread the stateful
    /// applier through. New code should prefer
    /// `LogicalChangeApplier::new()` + `apply()`. Apply misses (delete
    /// against a missing target) are recorded into a throwaway metrics
    /// handle; use [`apply_record_with_metrics`] to surface them.
    pub fn apply_record(db: &RedDB, record: &ChangeRecord, mode: ApplyMode) -> RedDBResult<()> {
        Self::apply_record_with_metrics(db, record, mode, &ReplicaApplyMetrics::default())
    }

    /// Stateless apply that records apply misses (issue #814) into
    /// `metrics`. A delete against a missing collection or a missing
    /// entity is a non-fatal divergence signal: it bumps
    /// `ApplyErrorKind::Miss` and emits a structured warn line, but still
    /// returns `Ok(())` so the LSN chain advances and idempotent re-pull
    /// (#813) converges. A genuine (non-missing-target) store error on a
    /// delete propagates as a real apply error — counted, fail-closed —
    /// rather than being swallowed by the old `let _ =`.
    pub fn apply_record_with_metrics(
        db: &RedDB,
        record: &ChangeRecord,
        _mode: ApplyMode,
        metrics: &ReplicaApplyMetrics,
    ) -> RedDBResult<()> {
        // Issue #1242 — compute payload bytes before the apply so we can
        // increment the throughput counter unconditionally at the end (early
        // `return Err` paths skip the counter, correctly counting only
        // successful applies).
        let payload_bytes: u64 = match record.operation {
            ChangeOperation::Insert | ChangeOperation::Update => record
                .entity_bytes
                .as_ref()
                .map(|b| b.len() as u64)
                .unwrap_or(0),
            ChangeOperation::Refresh => record
                .refresh_records
                .as_ref()
                .map(|recs| recs.iter().map(|r| r.len() as u64).sum())
                .unwrap_or(0),
            ChangeOperation::Delete => 0,
        };
        let store = db.store();
        match record.operation {
            ChangeOperation::Delete => {
                match store.delete(&record.collection, EntityId::new(record.entity_id)) {
                    Ok(true) => {}
                    Ok(false) => {
                        // Target collection existed but no such entity —
                        // the delete found nothing to remove.
                        metrics.record(ApplyErrorKind::Miss);
                        tracing::warn!(
                            target: "reddb::replication::apply",
                            lsn = record.lsn,
                            collection = %record.collection,
                            entity_id = record.entity_id,
                            "replica delete found no matching entity; recorded apply miss (non-fatal divergence signal)"
                        );
                    }
                    Err(crate::storage::StoreError::CollectionNotFound(name)) => {
                        // The whole collection is absent on the replica —
                        // a missed delete that can drive count drift.
                        metrics.record(ApplyErrorKind::Miss);
                        tracing::warn!(
                            target: "reddb::replication::apply",
                            lsn = record.lsn,
                            collection = %name,
                            entity_id = record.entity_id,
                            "replica delete against missing collection; recorded apply miss (non-fatal divergence signal)"
                        );
                    }
                    Err(err) => {
                        // A real store error is a genuine apply failure:
                        // surface it instead of discarding it so the
                        // caller counts it and the replica fails closed.
                        return Err(RedDBError::Internal(err.to_string()));
                    }
                }
            }
            ChangeOperation::Refresh => {
                // Issue #596 slice 9d — replica replay of
                // `REFRESH MATERIALIZED VIEW v`. The primary
                // emitted the serialized backing-collection
                // contents in `refresh_records`; apply the
                // atomic swap on the replica's local store
                // (which also persists a `RefreshCollection`
                // WAL action so the post-swap contents survive
                // a replica restart).
                let records = record.refresh_records.clone().ok_or_else(|| {
                    RedDBError::Internal(
                        "replication refresh record missing refresh_records payload".to_string(),
                    )
                })?;
                store
                    .refresh_collection_from_records(&record.collection, records)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }
            ChangeOperation::Insert | ChangeOperation::Update => {
                let Some(bytes) = &record.entity_bytes else {
                    return Err(RedDBError::Internal(
                        "replication record missing entity payload".to_string(),
                    ));
                };
                let entity = UnifiedStore::deserialize_entity(bytes, store.format_version())
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;

                // Issue #813 — MVCC table-row supersession on the replica.
                //
                // A table-row UPDATE on the primary installs a NEW physical
                // version (fresh `EntityId`) that shares the row's stable
                // `logical_id`, and marks the prior version superseded
                // (`xmax != 0`) so snapshot reads skip it. Only the new
                // version travels on the wire — the prior version's `xmax`
                // bump is implicit. Without reproducing it here the replica
                // leaves every prior version LIVE, so each update to a row
                // accumulates a stale live duplicate and a full re-pull
                // replays them all (the observed 22× inflation). Before
                // upserting the incoming version, mark any *other* live
                // version of the same logical id superseded — mirroring
                // `install_versioned_table_row_update` on the primary. This
                // is idempotent under re-pull: re-applying a record updates
                // its version in place (resetting its `xmax` from the
                // serialized bytes), and the last writer per logical id in
                // LSN order wins, converging on the primary's live set.
                if matches!(entity.kind, EntityKind::TableRow { .. }) {
                    let logical = entity.logical_id();
                    let new_id = entity.id;
                    let superseding_xid = if entity.xmin != 0 { entity.xmin } else { 1 };
                    let stale: Vec<_> = store
                        .table_row_versions_by_logical_id(&record.collection, logical)
                        .into_iter()
                        .filter(|version| version.id != new_id && version.xmax == 0)
                        .collect();
                    if !stale.is_empty() {
                        let manager = store
                            .get_collection(&record.collection)
                            .ok_or_else(|| RedDBError::NotFound(record.collection.clone()))?;
                        for mut version in stale {
                            version.set_xmax(superseding_xid);
                            manager
                                .update(version)
                                .map_err(|err| RedDBError::Internal(err.to_string()))?;
                        }
                    }
                }

                let exists = store
                    .get(&record.collection, EntityId::new(record.entity_id))
                    .is_some();
                if exists {
                    let manager = store
                        .get_collection(&record.collection)
                        .ok_or_else(|| RedDBError::NotFound(record.collection.clone()))?;
                    manager
                        .update(entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                } else {
                    store
                        .insert_auto(&record.collection, entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                if let Some(metadata_json) = &record.metadata {
                    let metadata_json = wire_json_to_server_json(metadata_json);
                    let metadata = metadata_from_json(&metadata_json)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                    store
                        .set_metadata(&record.collection, entity.id, metadata)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                store
                    .context_index()
                    .index_entity(&record.collection, &entity);
            }
        }
        // Issue #1242 — only reached on successful apply (all early `return
        // Err` paths skip this). Fenced and error records never reach here.
        metrics
            .bytes_applied_total
            .fetch_add(payload_bytes, Ordering::Relaxed);
        metrics
            .records_applied_total
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn record_payload_hash(record: &ChangeRecord) -> [u8; 32] {
    let mut hasher = crate::crypto::sha256::Sha256::new();
    hasher.update(&record.term.to_le_bytes());
    hasher.update(&record.lsn.to_le_bytes());
    hasher.update(&[record.operation as u8]);
    hasher.update(record.collection.as_bytes());
    hasher.update(&record.entity_id.to_le_bytes());
    // Issue #991 — range authority participates in the payload hash so two
    // records at the same LSN that differ only in range identity or ownership
    // epoch are flagged divergent rather than silently treated as idempotent.
    // `u64::MAX` stands in for an absent field (a value real epochs/ids never
    // reach) so `None` and `Some(MAX)` stay distinguishable.
    hasher.update(&record.range_id.unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(&record.ownership_epoch.unwrap_or(u64::MAX).to_le_bytes());
    if let Some(bytes) = &record.entity_bytes {
        hasher.update(bytes);
    }
    // Issue #596 slice 9d — refresh payload participates in the
    // payload-hash so the same-LSN-idempotent / different-payload-
    // divergence state machine works for Refresh records too.
    if let Some(records) = &record.refresh_records {
        hasher.update(&(records.len() as u64).to_le_bytes());
        for r in records {
            hasher.update(&(r.len() as u64).to_le_bytes());
            hasher.update(r);
        }
    }
    hasher.finalize()
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    crate::utils::to_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::cdc::ChangeOperation;
    use crate::storage::schema::Value;
    use crate::storage::{EntityData, EntityId, EntityKind, RedDB, RowData, UnifiedEntity};
    use std::sync::Arc;

    fn open_db() -> (RedDB, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "reddb_logical_apply_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let db = RedDB::open(&path).unwrap();
        (db, path)
    }

    fn record(lsn: u64, payload: &[u8]) -> ChangeRecord {
        let timestamp = 100 + lsn;
        let mut entity = UnifiedEntity::new(
            EntityId::new(lsn),
            EntityKind::TableRow {
                table: Arc::from("users"),
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
        change_record_from_entity(
            lsn,
            timestamp,
            ChangeOperation::Insert,
            "users",
            "row",
            &entity,
            crate::api::REDDB_FORMAT_VERSION,
            None,
        )
    }

    fn delete_record(lsn: u64, collection: &str, entity_id: u64) -> ChangeRecord {
        ChangeRecord {
            term: crate::replication::DEFAULT_REPLICATION_TERM,
            lsn,
            timestamp: 100 + lsn,
            operation: ChangeOperation::Delete,
            collection: collection.to_string(),
            entity_id,
            entity_kind: "row".to_string(),
            entity_bytes: None,
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        }
    }

    fn table_row_entity(id: u64) -> UnifiedEntity {
        let mut entity = UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TableRow {
                table: Arc::from("users"),
                row_id: id,
            },
            EntityData::Row(RowData::with_names(
                vec![Value::UnsignedInteger(id)],
                vec!["id".to_string()],
            )),
        );
        entity.created_at = 100 + id;
        entity.updated_at = 100 + id;
        entity.sequence_id = id;
        entity
    }

    // Issue #814 — a delete against a missing collection must record an
    // apply miss (not a silent no-op) and still return Ok so the LSN
    // chain advances (idempotent re-pull, #813).
    #[test]
    fn delete_against_missing_collection_records_apply_miss() {
        let (db, path) = open_db();
        let metrics = ReplicaApplyMetrics::default();
        let before = metrics.apply_miss_total.load(Ordering::Relaxed);

        LogicalChangeApplier::apply_record_with_metrics(
            &db,
            &delete_record(1, "no_such_collection", 42),
            ApplyMode::Replica,
            &metrics,
        )
        .expect("missing-target delete is non-fatal");

        assert_eq!(
            metrics.apply_miss_total.load(Ordering::Relaxed),
            before + 1,
            "delete against a missing collection must bump the apply-miss signal"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #814 — a delete against an existing collection but absent
    // entity is likewise a recorded miss, not a swallowed no-op.
    #[test]
    fn delete_against_missing_entity_records_apply_miss() {
        let (db, path) = open_db();
        let _ = db.store().get_or_create_collection("users");
        let metrics = ReplicaApplyMetrics::default();

        LogicalChangeApplier::apply_record_with_metrics(
            &db,
            &delete_record(1, "users", 9999),
            ApplyMode::Replica,
            &metrics,
        )
        .expect("missing-entity delete is non-fatal");

        assert_eq!(
            metrics.apply_miss_total.load(Ordering::Relaxed),
            1,
            "delete of an absent entity must bump the apply-miss signal"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #814 — the normal path (target present) deletes without
    // firing the miss signal. No behavioral regression.
    #[test]
    fn delete_of_present_target_records_no_apply_miss() {
        let (db, path) = open_db();
        let store = db.store();
        let _ = store.get_or_create_collection("users");
        let id = store
            .insert_auto("users", table_row_entity(1))
            .expect("insert entity");
        let metrics = ReplicaApplyMetrics::default();

        LogicalChangeApplier::apply_record_with_metrics(
            &db,
            &delete_record(1, "users", id.raw()),
            ApplyMode::Replica,
            &metrics,
        )
        .expect("present-target delete applies");

        assert_eq!(
            metrics.apply_miss_total.load(Ordering::Relaxed),
            0,
            "deleting a present target must not fire the apply-miss signal"
        );
        assert!(
            store.get("users", id).is_none(),
            "the entity must actually be removed on the normal path"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #814 — the shared-metrics handle on the stateful applier
    // surfaces the miss so `/metrics` (which reads the same Arc) sees it.
    #[test]
    fn stateful_apply_surfaces_delete_miss_via_metrics_handle() {
        let (db, path) = open_db();
        let applier =
            LogicalChangeApplier::with_metrics(0, Arc::new(ReplicaApplyMetrics::default()));

        applier
            .apply(&db, &delete_record(1, "ghost", 7), ApplyMode::Replica)
            .expect("missing-target delete advances the chain");

        assert_eq!(
            applier.metrics().apply_miss_total.load(Ordering::Relaxed),
            1,
            "the applier's shared metrics handle must record the miss"
        );
        assert_eq!(
            applier.last_applied_lsn(),
            1,
            "a non-fatal miss still advances the LSN chain"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #1242 — bytes_applied_total and records_applied_total must
    // reflect observed apply throughput: Applied increments both, Idempotent
    // and Skipped increment neither, and deletes contribute 0 bytes but 1
    // record.
    #[test]
    fn throughput_counters_reflect_applied_bytes_and_records() {
        let (db, path) = open_db();
        let metrics = Arc::new(ReplicaApplyMetrics::default());
        let applier = LogicalChangeApplier::with_metrics(0, Arc::clone(&metrics));

        // Three insert records with payloads of different sizes.
        let r1 = record(1, b"hello");
        let r2 = record(2, b"world-longer-payload");
        let r3 = record(3, b"x");

        // Compute expected bytes from the serialized entity_bytes (not the raw
        // payload, which goes through entity serialization before it lands in
        // entity_bytes).
        let expected_bytes: u64 = [&r1, &r2, &r3]
            .iter()
            .map(|r| r.entity_bytes.as_ref().map(|b| b.len() as u64).unwrap_or(0))
            .sum();
        assert!(expected_bytes > 0, "test records must carry entity payload");

        for r in [&r1, &r2, &r3] {
            assert_eq!(
                applier.apply(&db, r, ApplyMode::Replica).unwrap(),
                ApplyOutcome::Applied
            );
        }

        assert_eq!(
            metrics.records_applied_total.load(Ordering::Relaxed),
            3,
            "three applied records must increment records counter by 3"
        );
        assert_eq!(
            metrics.bytes_applied_total.load(Ordering::Relaxed),
            expected_bytes,
            "bytes counter must match the sum of entity_bytes across applied records"
        );

        // Idempotent replay must not increment either counter.
        assert_eq!(
            applier.apply(&db, &r3, ApplyMode::Replica).unwrap(),
            ApplyOutcome::Idempotent
        );
        assert_eq!(
            metrics.records_applied_total.load(Ordering::Relaxed),
            3,
            "idempotent replay must not increment the records counter"
        );
        assert_eq!(
            metrics.bytes_applied_total.load(Ordering::Relaxed),
            expected_bytes,
            "idempotent replay must not increment the bytes counter"
        );

        // A delete-miss (non-fatal) increments records by 1 but contributes
        // 0 bytes — deletes carry no entity payload.
        let del = delete_record(4, "ghost_collection", 999);
        applier
            .apply(&db, &del, ApplyMode::Replica)
            .expect("delete miss is non-fatal and still advances the chain");
        assert_eq!(
            metrics.records_applied_total.load(Ordering::Relaxed),
            4,
            "a delete (including a miss) must increment the records counter"
        );
        assert_eq!(
            metrics.bytes_applied_total.load(Ordering::Relaxed),
            expected_bytes,
            "a delete contributes 0 bytes to the bytes counter"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_advances_on_monotonic_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        assert_eq!(
            applier
                .apply(&db, &record(1, b"a"), ApplyMode::Replica)
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(applier.last_applied_lsn(), 1);
        assert_eq!(
            applier
                .apply(&db, &record(2, b"b"), ApplyMode::Replica)
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(applier.last_applied_lsn(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_idempotent_on_duplicate_lsn_same_payload() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        let r = record(5, b"same");
        applier.apply(&db, &r, ApplyMode::Replica).unwrap();
        assert_eq!(
            applier.apply(&db, &r, ApplyMode::Replica).unwrap(),
            ApplyOutcome::Idempotent
        );
        assert_eq!(applier.last_applied_lsn(), 5);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_fails_closed_on_lsn_collision_diff_payload() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier
            .apply(&db, &record(7, b"first"), ApplyMode::Replica)
            .unwrap();
        let err = applier
            .apply(&db, &record(7, b"different"), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::Divergence { lsn: 7, .. }),
            "got {err:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_fails_closed_on_same_lsn_different_term() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier
            .apply(&db, &record(7, b"same").with_term(1), ApplyMode::Replica)
            .unwrap();
        let err = applier
            .apply(&db, &record(7, b"same").with_term(2), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(
                err,
                LogicalApplyError::Divergence {
                    lsn: 7,
                    expected_term: 1,
                    got_term: 2,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert_eq!(applier.last_applied_term(), 1);
        assert_eq!(applier.last_applied_lsn(), 7);
        let _ = std::fs::remove_file(path);
    }

    // Issue #835 — a record from a term behind the replica's adopted term
    // is fenced at the apply boundary: rejected, counted, and crucially the
    // LSN/term chain does NOT advance, so a returning ex-primary on a stale
    // term cannot move any watermark.
    #[test]
    fn apply_fences_stale_term_record() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);

        // Replica adopts term 5 from the legitimate primary at lsn 1.
        applier
            .apply(&db, &record(1, b"a").with_term(5), ApplyMode::Replica)
            .unwrap();
        assert_eq!(applier.last_applied_term(), 5);
        assert_eq!(applier.last_applied_lsn(), 1);

        // A returning ex-primary streams the next record on the old term 4.
        let before = applier.metrics().fenced_total.load(Ordering::Relaxed);
        let err = applier
            .apply(&db, &record(2, b"b").with_term(4), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(
                err,
                LogicalApplyError::StaleTermFenced {
                    record_term: 4,
                    current_term: 5,
                    lsn: 2,
                }
            ),
            "got {err:?}"
        );
        assert_eq!(err.kind(), ApplyErrorKind::Fenced);
        assert_eq!(
            applier.metrics().fenced_total.load(Ordering::Relaxed),
            before + 1,
            "the fence must leave a metrics trail"
        );
        // The fenced record advanced nothing — no apply, no watermark move.
        assert_eq!(applier.last_applied_lsn(), 1, "watermark must not advance");
        assert_eq!(applier.last_applied_term(), 5);
        assert_eq!(
            applier.received_frontier_lsn(),
            1,
            "a fenced record must not even advance the received frontier"
        );
        let _ = std::fs::remove_file(path);
    }

    // The fence only bites a *behind* term. A record on the same term
    // applies normally, and a record on a higher term is the new primary's
    // timeline — adopted on apply.
    #[test]
    fn apply_admits_same_term_and_adopts_higher_term() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);

        applier
            .apply(&db, &record(1, b"a").with_term(3), ApplyMode::Replica)
            .unwrap();
        // Same term → admitted.
        applier
            .apply(&db, &record(2, b"b").with_term(3), ApplyMode::Replica)
            .unwrap();
        assert_eq!(applier.last_applied_term(), 3);
        // Higher term → adopted.
        applier
            .apply(&db, &record(3, b"c").with_term(7), ApplyMode::Replica)
            .unwrap();
        assert_eq!(applier.last_applied_term(), 7);
        assert_eq!(applier.last_applied_lsn(), 3);

        // Now a record back on term 3 is fenced.
        let err = applier
            .apply(&db, &record(4, b"d").with_term(3), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::StaleTermFenced { .. }),
            "got {err:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #991 — a record stamped for the target range but carrying an
    // ownership epoch behind the range's accepted epoch is a write from a
    // deposed owner. It is fenced at the apply boundary: rejected, counted on
    // the Fenced lane, and the LSN/term chain does not advance.
    #[test]
    fn apply_fenced_rejects_stale_ownership_epoch() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 1,
            min_ownership_epoch: 5,
        };

        let stale = record(1, b"a").with_range_authority(7, 4);
        let before = applier.metrics().fenced_total.load(Ordering::Relaxed);
        let err = applier
            .apply_fenced(&db, &stale, ApplyMode::Replica, Some(&fence))
            .unwrap_err();
        assert!(
            matches!(
                err,
                LogicalApplyError::RangeFenced {
                    range_id: 7,
                    lsn: 1,
                    reason: RangeAdmitError::StaleOwnershipEpoch {
                        record_epoch: 4,
                        accepted_epoch: 5,
                    },
                }
            ),
            "got {err:?}"
        );
        assert_eq!(err.kind(), ApplyErrorKind::Fenced);
        assert_eq!(
            applier.metrics().fenced_total.load(Ordering::Relaxed),
            before + 1
        );
        assert_eq!(applier.last_applied_lsn(), 0, "watermark must not advance");
        assert_eq!(
            applier.received_frontier_lsn(),
            0,
            "a range-fenced record must not advance the received frontier"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #991 — the same fence applies on the recovery/restore path: a
    // record on a stale term for the target range is rejected there too.
    #[test]
    fn apply_fenced_rejects_stale_range_term_on_restore() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        let fence = RangeAuthority {
            range_id: 3,
            min_term: 6,
            min_ownership_epoch: 1,
        };

        let stale = record(1, b"a").with_term(4).with_range_authority(3, 9);
        let err = applier
            .apply_fenced(&db, &stale, ApplyMode::Restore, Some(&fence))
            .unwrap_err();
        assert!(
            matches!(
                err,
                LogicalApplyError::RangeFenced {
                    range_id: 3,
                    reason: RangeAdmitError::StaleTerm {
                        record_term: 4,
                        accepted_term: 6,
                    },
                    ..
                }
            ),
            "got {err:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    // Issue #991 — a record current for the range applies through the fence,
    // and a record for a *different* range is not gated by this fence.
    #[test]
    fn apply_fenced_admits_current_and_ignores_other_ranges() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 1,
            min_ownership_epoch: 5,
        };

        // Current epoch for the fenced range → applies and advances.
        applier
            .apply_fenced(
                &db,
                &record(1, b"a").with_range_authority(7, 5),
                ApplyMode::Replica,
                Some(&fence),
            )
            .expect("current record applies");
        assert_eq!(applier.last_applied_lsn(), 1);

        // A record stamped for a different (stale-looking) range is not this
        // fence's concern and still applies.
        applier
            .apply_fenced(
                &db,
                &record(2, b"b").with_range_authority(99, 1),
                ApplyMode::Replica,
                Some(&fence),
            )
            .expect("other-range record bypasses this fence");
        assert_eq!(applier.last_applied_lsn(), 2);
        let _ = std::fs::remove_file(path);
    }

    // Issue #992 — end-to-end range-indexed catch-up over the single physical
    // WAL: a follower plans its range's work out of the shared stream, then
    // applies exactly that work through the same `apply_fenced` gate. The other
    // range's records and a deposed-owner write never reach apply.
    #[test]
    fn range_catchup_plan_applies_only_its_range_through_the_fence() {
        use crate::replication::cdc::{plan_range_catchup, RangeStreamPosition, RangeStreamReject};

        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);

        // One sequential WAL slice: range 7 at LSN 1..3 (epoch 5), range 9 at
        // 4..5, and a returning ex-owner of range 7 at LSN 6 on a stale epoch.
        let stream = vec![
            record(1, b"a").with_range_authority(7, 5),
            record(2, b"b").with_range_authority(7, 5),
            record(3, b"c").with_range_authority(7, 5),
            record(4, b"d").with_range_authority(9, 2),
            record(5, b"e").with_range_authority(9, 2),
            record(6, b"f").with_range_authority(7, 4),
        ];

        // Range-7 follower resumes from origin, already knowing owner epoch 5.
        let position = RangeStreamPosition::new(7, 0, 1, 5);
        let plan = plan_range_catchup(&position, &stream);

        // Only range 7's current records were selected; the stale-epoch write
        // is rejected, not selected.
        assert_eq!(plan.apply, vec![0, 1, 2]);
        assert_eq!(
            plan.rejected,
            vec![RangeStreamReject {
                lsn: 6,
                error: RangeAdmitError::StaleOwnershipEpoch {
                    record_epoch: 4,
                    accepted_epoch: 5,
                },
            }]
        );
        assert_eq!(plan.resume.applied_lsn, 3);

        // Apply exactly the planned records through the per-range fence.
        let fence = position.authority();
        for index in &plan.apply {
            applier
                .apply_fenced(&db, &stream[*index], ApplyMode::Replica, Some(&fence))
                .expect("planned record applies through the fence");
        }
        assert_eq!(applier.last_applied_lsn(), 3);

        // The deposed-owner record the plan refused is exactly what the apply
        // fence would also reject, never advancing the chain.
        let stale = &stream[5];
        let err = applier
            .apply_fenced(&db, stale, ApplyMode::Replica, Some(&fence))
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::RangeFenced { range_id: 7, .. }),
            "got {err:?}"
        );
        assert_eq!(
            applier.last_applied_lsn(),
            3,
            "stale write must not advance"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_skips_older_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier
            .apply(&db, &record(1, b"a"), ApplyMode::Replica)
            .unwrap();
        applier
            .apply(&db, &record(2, b"b"), ApplyMode::Replica)
            .unwrap();
        assert_eq!(
            applier
                .apply(&db, &record(1, b"a"), ApplyMode::Replica)
                .unwrap(),
            ApplyOutcome::Skipped
        );
        assert_eq!(applier.last_applied_lsn(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_returns_gap_on_future_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier
            .apply(&db, &record(1, b"a"), ApplyMode::Replica)
            .unwrap();
        let err = applier
            .apply(&db, &record(5, b"e"), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::Gap { last: 1, next: 5 }),
            "got {err:?}"
        );
        assert_eq!(applier.last_applied_lsn(), 1);
        let _ = std::fs::remove_file(path);
    }
}
