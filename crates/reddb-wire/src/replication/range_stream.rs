//! Range-indexed WAL streaming and per-range catch-up (issue #992).
//!
//! The physical WAL stays a single sequential append log — there is no
//! per-range physical file. Range replication is a *filtered view* over that
//! one log: every derived [`ChangeRecord`] carries its range identity, owning
//! term, and ownership epoch (issue #991), so a data member can index and route
//! records by range without splitting the log.
//!
//! This module supplies the transport-agnostic primitives a range replica or a
//! move-range target uses to ride that filtered view:
//!
//! - [`RangeStreamPosition`] — the per-range resume point and authority
//!   watermark a follower persists. Catch-up restarts from this position rather
//!   than from the global stream head.
//! - [`classify_range_record`] / [`plan_range_catchup`] — gate the shared
//!   stream down to exactly the records a range should apply: only records
//!   stamped for the range, only those past the resume LSN, only those whose
//!   term and ownership epoch clear the range authority (stale owners fenced).
//! - [`RangeStreamProgress`] / [`RangeProgressTracker`] — independent lag and
//!   progress per range over the one physical stream, enough to reason about
//!   per-range failover eligibility later (issue #987 parent).
//!
//! The actual on-disk apply (the LSN state machine, payload hashing) stays in
//! `reddb-server`; this crate only describes the routing/gating contract.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use super::change_record::{ChangeRecord, RangeAdmitError, RangeAuthority};
use super::util::{get_opt_u64, get_u64, object_from_slice, Result};

/// The per-range resume position and authority watermark a range follower
/// persists. A range replica restarting catch-up hands the primary this
/// position so streaming resumes from `applied_lsn` for the range instead of
/// replaying the whole shared WAL, and so the follower keeps fencing records
/// from a deposed owner.
///
/// `applied_lsn` is a *global* WAL LSN — the highest LSN this follower has
/// applied **for this range**. Because the range's records are sparse within
/// the shared sequential log, range catch-up admits any record with a strictly
/// greater LSN (range-local monotonicity) rather than requiring the global
/// `lsn == last + 1` contiguity the whole-stream applier enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeStreamPosition {
    pub range_id: u64,
    pub applied_lsn: u64,
    pub accepted_term: u64,
    pub accepted_epoch: u64,
}

impl RangeStreamPosition {
    pub fn new(range_id: u64, applied_lsn: u64, accepted_term: u64, accepted_epoch: u64) -> Self {
        Self {
            range_id,
            applied_lsn,
            accepted_term,
            accepted_epoch,
        }
    }

    /// A fresh follower for `range_id` that has applied nothing yet and holds
    /// the lowest possible authority watermark (accepts any term/epoch).
    pub fn at_origin(range_id: u64) -> Self {
        Self::new(range_id, 0, 0, 0)
    }

    /// The authority fence this position currently enforces. A record stamped
    /// for this range whose term or ownership epoch is below the watermark is a
    /// write from a stale timeline or deposed owner and is rejected.
    pub fn authority(&self) -> RangeAuthority {
        RangeAuthority {
            range_id: self.range_id,
            min_term: self.accepted_term,
            min_ownership_epoch: self.accepted_epoch,
        }
    }

    /// Advance this position past an admitted record that belongs to the range.
    /// The resume LSN moves forward and the authority watermark ratchets up to
    /// the record's term/epoch so a later stale write cannot slip back in. Only
    /// records stamped for this range and ahead of the current resume LSN move
    /// the position; everything else leaves it untouched.
    pub fn advance(&mut self, record: &ChangeRecord) {
        if record.range_id != Some(self.range_id) || record.lsn <= self.applied_lsn {
            return;
        }
        self.applied_lsn = record.lsn;
        if record.term > self.accepted_term {
            self.accepted_term = record.term;
        }
        if let Some(epoch) = record.ownership_epoch {
            if epoch > self.accepted_epoch {
                self.accepted_epoch = epoch;
            }
        }
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "range_id".to_string(),
            JsonValue::Number(self.range_id.into()),
        );
        obj.insert(
            "applied_lsn".to_string(),
            JsonValue::Number(self.applied_lsn.into()),
        );
        obj.insert(
            "accepted_term".to_string(),
            JsonValue::Number(self.accepted_term.into()),
        );
        obj.insert(
            "accepted_epoch".to_string(),
            JsonValue::Number(self.accepted_epoch.into()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            range_id: get_u64(&obj, "range_id")?,
            applied_lsn: get_opt_u64(&obj, "applied_lsn").unwrap_or(0),
            accepted_term: get_opt_u64(&obj, "accepted_term").unwrap_or(0),
            accepted_epoch: get_opt_u64(&obj, "accepted_epoch").unwrap_or(0),
        })
    }
}

/// Why a single record was routed the way it was during range catch-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeStreamDecision {
    /// Stamped for this range, ahead of the resume LSN, and clears the
    /// authority fence — apply it.
    Apply,
    /// Belongs to a different range (or carries no range identity at all) — not
    /// this follower's record, skip without touching its position.
    SkipOtherRange,
    /// Stamped for this range but at or below the resume LSN — already applied,
    /// skip idempotently.
    SkipReplayed,
    /// Stamped for this range but fenced: a stale term or ownership epoch from a
    /// deposed owner / superseded timeline.
    Reject(RangeAdmitError),
}

/// Route a single record relative to a range's resume position. Routing order:
/// other-range first (cheapest, most common), then already-applied, then the
/// authority fence — so a record we have already applied is skipped without
/// being treated as a fence violation, while a fresh stale-owner write is
/// rejected.
pub fn classify_range_record(
    position: &RangeStreamPosition,
    record: &ChangeRecord,
) -> RangeStreamDecision {
    if record.range_id != Some(position.range_id) {
        return RangeStreamDecision::SkipOtherRange;
    }
    if record.lsn <= position.applied_lsn {
        return RangeStreamDecision::SkipReplayed;
    }
    match position.authority().admit(record) {
        Ok(()) => RangeStreamDecision::Apply,
        Err(error) => RangeStreamDecision::Reject(error),
    }
}

/// A record that range catch-up refused, with the reason. Surfaced (rather than
/// silently dropped) so the caller can count fenced records and tell a stale
/// owner apart from a quiet range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeStreamReject {
    pub lsn: u64,
    pub error: RangeAdmitError,
}

/// The result of filtering a slice of the shared logical stream down to one
/// range's catch-up work. `apply` holds indices into the input slice in
/// ascending LSN order; `resume` is the position advanced past every applied
/// record (persist it to make catch-up resumable); `rejected` lists the fenced
/// records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCatchupPlan {
    pub range_id: u64,
    pub apply: Vec<usize>,
    pub rejected: Vec<RangeStreamReject>,
    pub resume: RangeStreamPosition,
    pub scanned: usize,
}

impl RangeCatchupPlan {
    /// Number of records selected for apply.
    pub fn apply_count(&self) -> usize {
        self.apply.len()
    }

    /// Whether anything was selected to apply.
    pub fn is_empty(&self) -> bool {
        self.apply.is_empty()
    }
}

/// Filter a slice of the shared logical stream (ascending by LSN) into the
/// catch-up plan for a single range, resuming from `position`. Only records
/// stamped for `position.range_id`, past its resume LSN, and clearing its
/// authority fence are selected; the returned `resume` position has been
/// advanced past them so a follower can persist it and continue.
///
/// The input is assumed LSN-ascending (the WAL is a sequential append log);
/// records are visited in order so the resume position ratchets monotonically.
pub fn plan_range_catchup(
    position: &RangeStreamPosition,
    records: &[ChangeRecord],
) -> RangeCatchupPlan {
    let mut resume = *position;
    let mut apply = Vec::new();
    let mut rejected = Vec::new();
    for (index, record) in records.iter().enumerate() {
        match classify_range_record(&resume, record) {
            RangeStreamDecision::Apply => {
                apply.push(index);
                resume.advance(record);
            }
            RangeStreamDecision::Reject(error) => rejected.push(RangeStreamReject {
                lsn: record.lsn,
                error,
            }),
            RangeStreamDecision::SkipOtherRange | RangeStreamDecision::SkipReplayed => {}
        }
    }
    RangeCatchupPlan {
        range_id: position.range_id,
        apply,
        rejected,
        resume,
        scanned: records.len(),
    }
}

/// Independent streaming progress for one range over the shared physical WAL.
///
/// All three LSNs are global WAL LSNs scoped to this range's records:
/// `primary_lsn` is the highest the primary has produced for the range,
/// `streamed_lsn` the highest shipped to the follower, `applied_lsn` the
/// highest the follower has durably applied. Their gaps give per-range lag that
/// is independent of every other range riding the same WAL — the basis for
/// per-range failover eligibility (issue #987).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeStreamProgress {
    pub range_id: u64,
    pub applied_lsn: u64,
    pub streamed_lsn: u64,
    pub primary_lsn: u64,
}

impl RangeStreamProgress {
    pub fn new(range_id: u64) -> Self {
        Self {
            range_id,
            applied_lsn: 0,
            streamed_lsn: 0,
            primary_lsn: 0,
        }
    }

    /// Records this range still has to apply to match the primary frontier.
    /// Saturating so a follower transiently ahead of an observed frontier
    /// reports zero rather than underflowing.
    pub fn apply_lag(&self) -> u64 {
        self.primary_lsn.saturating_sub(self.applied_lsn)
    }

    /// Records produced for this range that have not yet been shipped.
    pub fn stream_lag(&self) -> u64 {
        self.primary_lsn.saturating_sub(self.streamed_lsn)
    }

    /// Whether the follower has applied everything the primary has produced for
    /// the range. False until a primary frontier has actually been observed, so
    /// an unknown range is never reported as caught up.
    pub fn is_caught_up(&self) -> bool {
        self.primary_lsn > 0 && self.applied_lsn >= self.primary_lsn
    }

    /// Whether this range is within `max_lag` records of the primary — a
    /// per-range gate a later failover decision can consult. Requires an
    /// observed primary frontier.
    pub fn failover_eligible(&self, max_lag: u64) -> bool {
        self.primary_lsn > 0 && self.apply_lag() <= max_lag
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "range_id".to_string(),
            JsonValue::Number(self.range_id.into()),
        );
        obj.insert(
            "applied_lsn".to_string(),
            JsonValue::Number(self.applied_lsn.into()),
        );
        obj.insert(
            "streamed_lsn".to_string(),
            JsonValue::Number(self.streamed_lsn.into()),
        );
        obj.insert(
            "primary_lsn".to_string(),
            JsonValue::Number(self.primary_lsn.into()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            range_id: get_u64(&obj, "range_id")?,
            applied_lsn: get_opt_u64(&obj, "applied_lsn").unwrap_or(0),
            streamed_lsn: get_opt_u64(&obj, "streamed_lsn").unwrap_or(0),
            primary_lsn: get_opt_u64(&obj, "primary_lsn").unwrap_or(0),
        })
    }
}

/// Tracks streaming progress for many ranges over the one physical WAL. A data
/// member feeds every derived record through [`index_record`](Self::index_record)
/// to maintain each range's primary frontier without splitting the log, and
/// notes shipped/applied LSNs per range. Every range advances independently, so
/// one lagging range does not skew another's lag or failover eligibility.
///
/// All updates are monotonic (`max`): out-of-order or replayed observations
/// never move a frontier backward.
#[derive(Debug, Clone, Default)]
pub struct RangeProgressTracker {
    ranges: BTreeMap<u64, RangeStreamProgress>,
}

impl RangeProgressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&mut self, range_id: u64) -> &mut RangeStreamProgress {
        self.ranges
            .entry(range_id)
            .or_insert_with(|| RangeStreamProgress::new(range_id))
    }

    /// Index one derived record by its range identity, bumping that range's
    /// primary frontier. Records that carry no range identity (legacy /
    /// non-range-replicated) are ignored — they belong to no range's stream.
    pub fn index_record(&mut self, record: &ChangeRecord) {
        let Some(range_id) = record.range_id else {
            return;
        };
        let slot = self.slot(range_id);
        if record.lsn > slot.primary_lsn {
            slot.primary_lsn = record.lsn;
        }
    }

    /// Note that records for `range_id` up to `lsn` have been shipped to the
    /// follower. Also raises the primary frontier if it lagged behind, since you
    /// cannot stream past what was produced.
    pub fn note_streamed(&mut self, range_id: u64, lsn: u64) {
        let slot = self.slot(range_id);
        if lsn > slot.streamed_lsn {
            slot.streamed_lsn = lsn;
        }
        if lsn > slot.primary_lsn {
            slot.primary_lsn = lsn;
        }
    }

    /// Note that the follower has durably applied records for `range_id` up to
    /// `lsn`. Raises the streamed and primary frontiers if they lagged, since an
    /// applied record was necessarily streamed and produced.
    pub fn note_applied(&mut self, range_id: u64, lsn: u64) {
        let slot = self.slot(range_id);
        if lsn > slot.applied_lsn {
            slot.applied_lsn = lsn;
        }
        if lsn > slot.streamed_lsn {
            slot.streamed_lsn = lsn;
        }
        if lsn > slot.primary_lsn {
            slot.primary_lsn = lsn;
        }
    }

    /// Adopt a follower's reported position for `range_id` as the applied
    /// frontier — the inbound counterpart to [`note_applied`](Self::note_applied)
    /// when a follower acks with a [`RangeStreamPosition`].
    pub fn observe_position(&mut self, position: &RangeStreamPosition) {
        self.note_applied(position.range_id, position.applied_lsn);
    }

    pub fn progress(&self, range_id: u64) -> Option<&RangeStreamProgress> {
        self.ranges.get(&range_id)
    }

    /// Apply lag for one range, or `None` if the range is unknown.
    pub fn apply_lag(&self, range_id: u64) -> Option<u64> {
        self.ranges
            .get(&range_id)
            .map(RangeStreamProgress::apply_lag)
    }

    /// Iterate every tracked range's progress, ascending by range id.
    pub fn iter(&self) -> impl Iterator<Item = &RangeStreamProgress> {
        self.ranges.values()
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The ranges currently within `max_lag` of their primary frontier —
    /// candidates a later per-range failover decision may promote. Ascending by
    /// range id.
    pub fn failover_eligible(&self, max_lag: u64) -> Vec<u64> {
        self.ranges
            .values()
            .filter(|progress| progress.failover_eligible(max_lag))
            .map(|progress| progress.range_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::change_record::ChangeOperation;

    fn record(range_id: Option<u64>, lsn: u64, term: u64, epoch: Option<u64>) -> ChangeRecord {
        ChangeRecord {
            term,
            lsn,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "c".to_string(),
            entity_id: lsn,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1]),
            metadata: None,
            refresh_records: None,
            range_id,
            ownership_epoch: epoch,
        }
    }

    #[test]
    fn position_round_trips_on_the_json_wire() {
        let pos = RangeStreamPosition::new(7, 42, 3, 5);
        assert_eq!(
            RangeStreamPosition::decode_json(&pos.encode_json()).unwrap(),
            pos
        );
    }

    #[test]
    fn classify_routes_by_range_identity() {
        // Resume at origin for range 7.
        let pos = RangeStreamPosition::at_origin(7);
        // A record for range 7 is applied.
        assert_eq!(
            classify_range_record(&pos, &record(Some(7), 1, 1, Some(1))),
            RangeStreamDecision::Apply
        );
        // A record for a different range is not this follower's business.
        assert_eq!(
            classify_range_record(&pos, &record(Some(9), 1, 1, Some(1))),
            RangeStreamDecision::SkipOtherRange
        );
        // A legacy record with no range identity is skipped, not applied.
        assert_eq!(
            classify_range_record(&pos, &record(None, 1, 1, None)),
            RangeStreamDecision::SkipOtherRange
        );
    }

    #[test]
    fn plan_filters_one_range_out_of_a_shared_stream() {
        // A single physical WAL slice interleaving two ranges.
        let stream = vec![
            record(Some(7), 1, 1, Some(1)),
            record(Some(9), 2, 1, Some(1)),
            record(Some(7), 3, 1, Some(1)),
            record(None, 4, 1, None), // legacy / non-range
            record(Some(7), 5, 1, Some(1)),
        ];
        let plan = plan_range_catchup(&RangeStreamPosition::at_origin(7), &stream);
        // Only range 7's records, in LSN order.
        assert_eq!(plan.apply, vec![0, 2, 4]);
        assert!(plan.rejected.is_empty());
        assert_eq!(plan.scanned, 5);
        // Resume position advanced to range 7's highest applied LSN.
        assert_eq!(plan.resume.applied_lsn, 5);
        assert_eq!(plan.apply_count(), 3);
    }

    #[test]
    fn plan_resumes_from_a_known_range_position() {
        let stream = vec![
            record(Some(7), 1, 1, Some(1)),
            record(Some(7), 3, 1, Some(1)),
            record(Some(7), 5, 1, Some(1)),
        ];
        // Already applied through LSN 3 for range 7.
        let pos = RangeStreamPosition::new(7, 3, 1, 1);
        let plan = plan_range_catchup(&pos, &stream);
        // LSN 1 and 3 are replayed-skipped; only 5 applies.
        assert_eq!(plan.apply, vec![2]);
        assert_eq!(plan.resume.applied_lsn, 5);
    }

    #[test]
    fn plan_rejects_stale_ownership_epoch_and_term() {
        // Follower accepts term >= 3, epoch >= 4 for range 7.
        let pos = RangeStreamPosition::new(7, 0, 3, 4);
        let stream = vec![
            record(Some(7), 1, 3, Some(2)), // stale epoch
            record(Some(7), 2, 1, Some(9)), // stale term (checked first)
            record(Some(7), 3, 3, Some(4)), // current — admitted
        ];
        let plan = plan_range_catchup(&pos, &stream);
        assert_eq!(plan.apply, vec![2]);
        assert_eq!(
            plan.rejected,
            vec![
                RangeStreamReject {
                    lsn: 1,
                    error: RangeAdmitError::StaleOwnershipEpoch {
                        record_epoch: 2,
                        accepted_epoch: 4,
                    },
                },
                RangeStreamReject {
                    lsn: 2,
                    error: RangeAdmitError::StaleTerm {
                        record_term: 1,
                        accepted_term: 3,
                    },
                },
            ]
        );
        // The rejected stale writes never moved the resume position.
        assert_eq!(plan.resume.applied_lsn, 3);
        assert_eq!(plan.resume.accepted_epoch, 4);
    }

    #[test]
    fn position_advance_ratchets_authority_so_a_later_stale_write_is_fenced() {
        let mut pos = RangeStreamPosition::new(7, 0, 1, 1);
        // Apply a record that lifts the range to term 4, epoch 6.
        pos.advance(&record(Some(7), 10, 4, Some(6)));
        assert_eq!(pos.applied_lsn, 10);
        assert_eq!(pos.accepted_term, 4);
        assert_eq!(pos.accepted_epoch, 6);
        // A returning ex-owner at the old epoch is now fenced.
        assert_eq!(
            classify_range_record(&pos, &record(Some(7), 11, 4, Some(5))),
            RangeStreamDecision::Reject(RangeAdmitError::StaleOwnershipEpoch {
                record_epoch: 5,
                accepted_epoch: 6,
            })
        );
    }

    #[test]
    fn progress_round_trips_and_reports_lag() {
        let mut progress = RangeStreamProgress::new(7);
        progress.primary_lsn = 100;
        progress.streamed_lsn = 80;
        progress.applied_lsn = 60;
        assert_eq!(progress.apply_lag(), 40);
        assert_eq!(progress.stream_lag(), 20);
        assert!(!progress.is_caught_up());
        assert!(progress.failover_eligible(50));
        assert!(!progress.failover_eligible(10));
        assert_eq!(
            RangeStreamProgress::decode_json(&progress.encode_json()).unwrap(),
            progress
        );
    }

    #[test]
    fn tracker_reports_independent_lag_for_multiple_ranges() {
        let mut tracker = RangeProgressTracker::new();
        // Index a shared WAL slice spanning ranges 7 and 9.
        for rec in [
            record(Some(7), 1, 1, Some(1)),
            record(Some(9), 2, 1, Some(1)),
            record(Some(7), 3, 1, Some(1)),
            record(Some(9), 4, 1, Some(1)),
            record(None, 5, 1, None), // ignored by the index
            record(Some(9), 6, 1, Some(1)),
        ] {
            tracker.index_record(&rec);
        }
        // Range 7 has fully applied; range 9 lags behind.
        tracker.note_applied(7, 3);
        tracker.note_applied(9, 2);

        assert_eq!(tracker.len(), 2);
        // Range 7: primary frontier 3, applied 3 → caught up, zero lag.
        assert_eq!(tracker.apply_lag(7), Some(0));
        assert!(tracker.progress(7).unwrap().is_caught_up());
        // Range 9: primary frontier 6, applied 2 → lag 4, independent of range 7.
        assert_eq!(tracker.apply_lag(9), Some(4));
        assert!(!tracker.progress(9).unwrap().is_caught_up());
        // The legacy record minted no range slot.
        assert_eq!(tracker.apply_lag(99), None);

        // Only the caught-up range is failover-eligible at a tight bound; the
        // lagging range joins once the bound widens.
        assert_eq!(tracker.failover_eligible(0), vec![7]);
        assert_eq!(tracker.failover_eligible(10), vec![7, 9]);
    }

    #[test]
    fn tracker_frontiers_are_monotonic() {
        let mut tracker = RangeProgressTracker::new();
        tracker.note_applied(7, 50);
        // A stale, out-of-order observation must not move anything backward.
        tracker.note_streamed(7, 10);
        tracker.note_applied(7, 20);
        tracker.index_record(&record(Some(7), 5, 1, Some(1)));
        let progress = tracker.progress(7).unwrap();
        assert_eq!(progress.applied_lsn, 50);
        assert_eq!(progress.streamed_lsn, 50);
        assert_eq!(progress.primary_lsn, 50);
    }

    #[test]
    fn observe_position_adopts_follower_applied_frontier() {
        let mut tracker = RangeProgressTracker::new();
        tracker.index_record(&record(Some(7), 9, 1, Some(1)));
        tracker.observe_position(&RangeStreamPosition::new(7, 7, 1, 1));
        assert_eq!(tracker.apply_lag(7), Some(2));
    }
}
