//! Auto-rollback of a deposed primary to the common point (issue #840,
//! PRD #819, ADR 0030).
//!
//! When a former primary rejoins after a failover still holding writes
//! above the point its log last agreed with the new primary — a
//! *divergent tail* — it must drop that tail to rejoin a single timeline.
//! The tail is, by definition, **non-committed**: it sits above the
//! commit watermark (the highest LSN durably replicated to a quorum), so
//! removing it from the live timeline is correct (ADR 0030,
//! `NeverRollbackCommitted`).
//!
//! This module is the *recover-to-LSN* mechanism that does that drop:
//!
//! 1. **Plan & guard the boundary.** The recover target is the *common
//!    point* — the LSN up to which the deposed primary's log still agrees
//!    with the new primary (produced by the election, #834). The hard
//!    invariant is that the common point is **at or above the commit
//!    watermark** (#822): nothing at or below the watermark is ever rolled
//!    back. If the common point is below the watermark, the coordinator
//!    **refuses** to roll back rather than destroy committed data.
//! 2. **Preserve the tail.** Read the divergent tail and persist it to a
//!    rollback file *before* anything is removed. Rollback is never
//!    silent: if the tail cannot be persisted, the recovery aborts and no
//!    data is dropped.
//! 3. **Recover-to-LSN.** Roll the live timeline back to the common point
//!    over the MVCC history store (ADR 0014), discarding the tail's
//!    versions and restoring the pre-images visible at the common point.
//! 4. **Surface a loud operator event** so the discarded writes stay
//!    auditable and reconcilable.
//! 5. **Rejoin as a replica** of the new primary under the new term.
//!
//! ## Module shape
//!
//! [`RollbackCoordinator::run`] is a pure state machine. The boundary
//! math ([`RollbackPlan::compute`]) is separated out so the invariant can
//! be asserted in isolation. Every side effect — reading the tail,
//! writing the rollback file, the MVCC recover-to-LSN, the operator
//! event, the role swap — is injected behind [`RollbackTransport`], so
//! the whole flow runs deterministically against a scripted fake with no
//! engine, disk, clock, or network dependency. Wiring the transport onto
//! the real MVCC history store and the gRPC role-swap belongs to the
//! transport layer once the election (#834) and stale-term fencing (#835)
//! are live; this slice builds and proves the mechanism in isolation.

use super::failover::NodeRole;

/// A single record from the divergent tail that is about to be discarded.
///
/// Carries enough to reconstruct the write for an operator who later
/// reconciles a rollback file: its LSN, the term that produced it, and the
/// opaque record payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailRecord {
    /// LSN of this record on the deposed primary's local timeline.
    pub lsn: u64,
    /// The replication term under which the deposed primary wrote it.
    pub term: u64,
    /// Opaque encoded record bytes, preserved verbatim in the rollback
    /// file.
    pub payload: Vec<u8>,
}

impl TailRecord {
    pub fn new(lsn: u64, term: u64, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            lsn,
            term,
            payload: payload.into(),
        }
    }
}

/// The divergent tail removed from the live timeline: the records in
/// `(common_point_lsn, to_lsn]` that never reached quorum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergentTail {
    /// The common point — exclusive lower bound. Records at or below this
    /// LSN are kept; this is the recover-to-LSN target.
    pub common_point_lsn: u64,
    /// Inclusive upper bound — the deposed primary's local frontier.
    pub to_lsn: u64,
    /// The tail records, in LSN order. May be shorter than the LSN span
    /// (e.g. sparse / coalesced records); the span is authoritative for
    /// the boundary, the records are what gets preserved.
    pub records: Vec<TailRecord>,
}

impl DivergentTail {
    /// Number of LSNs removed from the live timeline.
    pub fn span_lsns(&self) -> u64 {
        self.to_lsn.saturating_sub(self.common_point_lsn)
    }
}

/// The computed, side-effect-free rollback plan. Splitting this out lets
/// the boundary invariant be asserted without driving any transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    /// The recover-to-LSN target: the common point with the new primary.
    pub recover_to_lsn: u64,
    /// The deposed primary's local frontier (inclusive tail upper bound).
    pub local_frontier: u64,
    /// The commit watermark — the durable floor that bounds the recover
    /// target from below.
    pub commit_watermark: u64,
    /// Number of LSNs the tail spans (`local_frontier - recover_to_lsn`).
    pub tail_lsns: u64,
}

impl RollbackPlan {
    /// Compute and validate the rollback plan for `req`.
    ///
    /// Enforces the hard invariant: the recover target (common point)
    /// must be **at or above** the commit watermark, so nothing at or
    /// below the watermark is ever rolled back. A common point below the
    /// watermark means the election handed us a target that would discard
    /// committed data — a contract violation we refuse rather than honour.
    pub fn compute(req: &RollbackRequest) -> Result<Self, RollbackError> {
        if req.common_point < req.commit_watermark {
            return Err(RollbackError::WatermarkViolation {
                common_point: req.common_point,
                commit_watermark: req.commit_watermark,
            });
        }
        Ok(Self {
            recover_to_lsn: req.common_point,
            local_frontier: req.local_frontier,
            commit_watermark: req.commit_watermark,
            tail_lsns: req.local_frontier.saturating_sub(req.common_point),
        })
    }

    /// Whether there is a divergent tail to roll back. When the local
    /// frontier is at or below the common point the node is already on the
    /// shared timeline and only needs to rejoin.
    pub fn has_divergent_tail(&self) -> bool {
        self.local_frontier > self.recover_to_lsn
    }
}

/// A request to auto-rollback a deposed primary to the common point and
/// rejoin it as a replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    /// The deposed primary's highest local LSN (tail upper bound).
    pub local_frontier: u64,
    /// The common point with the new primary — the recover-to-LSN target,
    /// produced by the election (#834).
    pub common_point: u64,
    /// The commit watermark — the highest LSN durably replicated to a
    /// quorum (#822). The recover target may never fall below this.
    pub commit_watermark: u64,
    /// Dial address of the new primary the node rejoins as a replica of.
    pub new_primary_addr: String,
    /// The term the new primary serves; the rejoining replica follows it.
    pub new_term: u64,
}

/// The loud operator event payload describing a completed rollback,
/// handed to [`RollbackTransport::emit_rollback_event`]. Mirrors
/// [`crate::telemetry::operator_event::OperatorEvent::DeposedPrimaryRollback`]
/// so the production transport can forward it verbatim while a test
/// transport can capture it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackEvent {
    pub common_point_lsn: u64,
    pub tail_to_lsn: u64,
    pub tail_lsns: u64,
    pub commit_watermark: u64,
    pub rollback_file: String,
    pub new_primary_addr: String,
    pub new_term: u64,
}

/// The result of a completed rejoin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    /// The LSN the node recovered to — the common point.
    pub recovered_to_lsn: u64,
    /// Number of LSNs removed from the live timeline (`0` when there was
    /// no divergent tail).
    pub tail_lsns: u64,
    /// Where the discarded tail was preserved. `None` only when there was
    /// no tail to preserve.
    pub rollback_file: Option<String>,
    /// Whether the loud operator event fired. Always `true` when a tail
    /// was discarded; `false` for a clean rejoin with no tail.
    pub event_fired: bool,
    /// The role the node now plays — a replica of the new primary under
    /// the new term.
    pub role: NodeRole,
}

impl RollbackOutcome {
    /// True when a divergent tail was actually rolled back (as opposed to
    /// a clean rejoin with nothing to discard).
    pub fn rolled_back_tail(&self) -> bool {
        self.tail_lsns > 0
    }
}

/// Why an auto-rollback could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackError {
    /// The common point is below the commit watermark, so recovering to it
    /// would roll back committed data. Refused — nothing was changed. This
    /// should never happen given the election vote rule (ADR 0030); if it
    /// does, the cluster has a deeper consistency fault that needs an
    /// operator, not a silent data loss.
    WatermarkViolation {
        common_point: u64,
        commit_watermark: u64,
    },
    /// The divergent tail could not be persisted to a rollback file.
    /// Recovery aborted **before** removing anything: rollback is never
    /// silent, so if the tail cannot be preserved it is not discarded.
    TailPersistFailed { reason: String },
    /// The recover-to-LSN over the MVCC history store failed. The tail was
    /// already preserved to a rollback file, but the live timeline was not
    /// rolled back; the node must not rejoin until an operator resolves it.
    RecoverFailed { target_lsn: u64, reason: String },
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RollbackError::WatermarkViolation {
                common_point,
                commit_watermark,
            } => write!(
                f,
                "auto-rollback refused: common point {common_point} is below the commit watermark \
                 {commit_watermark}; recovering to it would roll back committed data",
            ),
            RollbackError::TailPersistFailed { reason } => write!(
                f,
                "auto-rollback aborted: could not persist divergent tail to a rollback file \
                 ({reason}); nothing was rolled back",
            ),
            RollbackError::RecoverFailed { target_lsn, reason } => write!(
                f,
                "auto-rollback failed: recover-to-LSN {target_lsn} over the MVCC history store \
                 failed ({reason}); the divergent tail was preserved but the timeline was not \
                 rolled back",
            ),
        }
    }
}

impl std::error::Error for RollbackError {}

/// Side effects the rollback coordinator drives, injected so the state
/// machine stays pure and deterministically testable.
///
/// Production implementors back these onto the MVCC history store
/// (ADR 0014) for the recover-to-LSN, the rollback-file writer, the
/// [`crate::telemetry::operator_event::OperatorEvent`] bus, and the gRPC
/// role-swap. Tests back them onto a scripted fake.
pub trait RollbackTransport {
    /// Read the divergent tail records in `(from_exclusive, to_inclusive]`
    /// from the local timeline / MVCC history store, in LSN order.
    fn read_divergent_tail(&mut self, from_exclusive: u64, to_inclusive: u64) -> Vec<TailRecord>;

    /// Persist the divergent tail to a durable rollback file and return a
    /// path/handle that identifies it. Returning `Err` aborts the
    /// rollback **before** any data is removed — rollback is never silent.
    fn persist_rollback_file(&mut self, tail: &DivergentTail) -> Result<String, String>;

    /// Recover the live timeline to `target_lsn` over the MVCC history
    /// store, discarding every version above it and restoring the
    /// pre-images visible at `target_lsn`.
    fn recover_to_lsn(&mut self, target_lsn: u64) -> Result<(), String>;

    /// Emit the loud, auditable operator event for the completed rollback.
    fn emit_rollback_event(&mut self, event: RollbackEvent);

    /// Reconfigure the node to stream as a replica of `primary_addr` under
    /// `term`.
    fn rejoin_as_replica(&mut self, primary_addr: &str, term: u64);
}

/// The deposed-primary auto-rollback state machine.
pub struct RollbackCoordinator;

impl RollbackCoordinator {
    /// Execute the auto-rollback described by `req`, driving the node
    /// through `tx`.
    ///
    /// Ordering is chosen so the hard guarantees hold even on partial
    /// failure:
    ///
    /// 1. Compute & guard the boundary — refuse if it would cross the
    ///    watermark, changing nothing.
    /// 2. If there is no divergent tail, just rejoin.
    /// 3. Read and **persist** the tail before removing anything; abort if
    ///    it cannot be preserved.
    /// 4. Recover-to-LSN to the common point.
    /// 5. Fire the loud operator event.
    /// 6. Rejoin as a replica of the new primary.
    pub fn run(
        req: &RollbackRequest,
        tx: &mut dyn RollbackTransport,
    ) -> Result<RollbackOutcome, RollbackError> {
        let plan = RollbackPlan::compute(req)?;

        let role = NodeRole::Replica {
            primary_addr: req.new_primary_addr.clone(),
            term: req.new_term,
        };

        // No divergent tail: the node is already on the shared timeline.
        // Just rejoin — nothing to preserve, nothing to roll back, no
        // operator event.
        if !plan.has_divergent_tail() {
            tx.rejoin_as_replica(&req.new_primary_addr, req.new_term);
            return Ok(RollbackOutcome {
                recovered_to_lsn: plan.recover_to_lsn,
                tail_lsns: 0,
                rollback_file: None,
                event_fired: false,
                role,
            });
        }

        // Read the tail and preserve it BEFORE removing anything. If we
        // cannot persist it, abort without rolling back — rollback is
        // never silent.
        let records = tx.read_divergent_tail(plan.recover_to_lsn, plan.local_frontier);
        let tail = DivergentTail {
            common_point_lsn: plan.recover_to_lsn,
            to_lsn: plan.local_frontier,
            records,
        };
        let rollback_file = tx
            .persist_rollback_file(&tail)
            .map_err(|reason| RollbackError::TailPersistFailed { reason })?;

        // Recover the live timeline to the common point over the MVCC
        // history store. The tail is already safe in the rollback file.
        tx.recover_to_lsn(plan.recover_to_lsn)
            .map_err(|reason| RollbackError::RecoverFailed {
                target_lsn: plan.recover_to_lsn,
                reason,
            })?;

        // Surface the discarded writes loudly so they stay auditable.
        tx.emit_rollback_event(RollbackEvent {
            common_point_lsn: plan.recover_to_lsn,
            tail_to_lsn: plan.local_frontier,
            tail_lsns: plan.tail_lsns,
            commit_watermark: plan.commit_watermark,
            rollback_file: rollback_file.clone(),
            new_primary_addr: req.new_primary_addr.clone(),
            new_term: req.new_term,
        });

        // Rejoin as a replica of the new primary under the new term.
        tx.rejoin_as_replica(&req.new_primary_addr, req.new_term);

        Ok(RollbackOutcome {
            recovered_to_lsn: plan.recover_to_lsn,
            tail_lsns: plan.tail_lsns,
            rollback_file: Some(rollback_file),
            event_fired: true,
            role,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted fake recording every side effect so tests can assert
    /// ordering and content. `persist_should_fail` / `recover_should_fail`
    /// drive the abort paths.
    struct FakeTransport {
        /// Records the fake hands back from `read_divergent_tail`.
        available_tail: Vec<TailRecord>,
        persist_should_fail: bool,
        recover_should_fail: bool,
        // Captured effects, in order.
        persisted: Option<DivergentTail>,
        recovered_to: Option<u64>,
        emitted: Option<RollbackEvent>,
        rejoined: Option<(String, u64)>,
        /// Order log of effect names, to assert preserve-before-recover.
        order: Vec<&'static str>,
    }

    impl FakeTransport {
        fn new(available_tail: Vec<TailRecord>) -> Self {
            Self {
                available_tail,
                persist_should_fail: false,
                recover_should_fail: false,
                persisted: None,
                recovered_to: None,
                emitted: None,
                rejoined: None,
                order: Vec::new(),
            }
        }
    }

    impl RollbackTransport for FakeTransport {
        fn read_divergent_tail(
            &mut self,
            from_exclusive: u64,
            to_inclusive: u64,
        ) -> Vec<TailRecord> {
            self.order.push("read");
            self.available_tail
                .iter()
                .filter(|r| r.lsn > from_exclusive && r.lsn <= to_inclusive)
                .cloned()
                .collect()
        }

        fn persist_rollback_file(&mut self, tail: &DivergentTail) -> Result<String, String> {
            self.order.push("persist");
            if self.persist_should_fail {
                return Err("disk full".to_string());
            }
            self.persisted = Some(tail.clone());
            Ok(format!(
                "/data/rollback/lsn-{}-{}.rbk",
                tail.common_point_lsn, tail.to_lsn
            ))
        }

        fn recover_to_lsn(&mut self, target_lsn: u64) -> Result<(), String> {
            self.order.push("recover");
            if self.recover_should_fail {
                return Err("history truncated".to_string());
            }
            self.recovered_to = Some(target_lsn);
            Ok(())
        }

        fn emit_rollback_event(&mut self, event: RollbackEvent) {
            self.order.push("emit");
            self.emitted = Some(event);
        }

        fn rejoin_as_replica(&mut self, primary_addr: &str, term: u64) {
            self.order.push("rejoin");
            self.rejoined = Some((primary_addr.to_string(), term));
        }
    }

    fn request(local_frontier: u64, common_point: u64, watermark: u64) -> RollbackRequest {
        RollbackRequest {
            local_frontier,
            common_point,
            commit_watermark: watermark,
            new_primary_addr: "http://node-b:55055".to_string(),
            new_term: 8,
        }
    }

    fn tail(lsns: &[u64], term: u64) -> Vec<TailRecord> {
        lsns.iter()
            .map(|lsn| TailRecord::new(*lsn, term, vec![*lsn as u8]))
            .collect()
    }

    // ------------------------------------------------------------------
    // Boundary math (pure plan)
    // ------------------------------------------------------------------

    #[test]
    fn plan_recovers_to_common_point_and_sizes_the_tail() {
        let plan = RollbackPlan::compute(&request(230, 200, 200)).expect("valid plan");
        assert_eq!(
            plan.recover_to_lsn, 200,
            "recover target is the common point"
        );
        assert_eq!(plan.tail_lsns, 30, "tail spans common_point..frontier");
        assert!(plan.has_divergent_tail());
    }

    #[test]
    fn plan_with_common_point_above_watermark_is_allowed() {
        // The common point may sit ABOVE the watermark — the deposed
        // primary agreed with the new primary past the durable floor.
        // Only what is above the common point is rolled back.
        let plan = RollbackPlan::compute(&request(300, 250, 200)).expect("valid plan");
        assert_eq!(plan.recover_to_lsn, 250);
        assert_eq!(plan.tail_lsns, 50);
    }

    #[test]
    fn plan_refuses_common_point_below_watermark() {
        // HARD INVARIANT: nothing at or below the commit watermark is ever
        // rolled back. A common point below the watermark would do exactly
        // that, so the plan is refused.
        let err = RollbackPlan::compute(&request(300, 150, 200)).expect_err("must refuse");
        assert_eq!(
            err,
            RollbackError::WatermarkViolation {
                common_point: 150,
                commit_watermark: 200,
            }
        );
    }

    #[test]
    fn plan_at_watermark_is_the_inclusive_floor() {
        // common_point == watermark is allowed: the watermark itself is
        // kept, only strictly-above records are rolled back.
        let plan = RollbackPlan::compute(&request(220, 200, 200)).expect("valid at floor");
        assert_eq!(plan.recover_to_lsn, 200);
        assert_eq!(plan.tail_lsns, 20);
    }

    // ------------------------------------------------------------------
    // Full run: happy path
    // ------------------------------------------------------------------

    #[test]
    fn run_preserves_tail_then_recovers_then_emits_then_rejoins() {
        let mut tx = FakeTransport::new(tail(&[201, 210, 230], 7));
        let outcome =
            RollbackCoordinator::run(&request(230, 200, 200), &mut tx).expect("rollback succeeds");

        // Boundary: recovered to the common point, tail sized correctly.
        assert_eq!(outcome.recovered_to_lsn, 200);
        assert_eq!(outcome.tail_lsns, 30);
        assert!(outcome.rolled_back_tail());

        // Tail preserved: the rollback file holds exactly the records
        // above the common point.
        let persisted = tx.persisted.as_ref().expect("tail persisted");
        assert_eq!(persisted.common_point_lsn, 200);
        assert_eq!(persisted.to_lsn, 230);
        assert_eq!(persisted.records, tail(&[201, 210, 230], 7));
        assert_eq!(
            outcome.rollback_file.as_deref(),
            Some("/data/rollback/lsn-200-230.rbk")
        );

        // Recover-to-LSN hit the common point.
        assert_eq!(tx.recovered_to, Some(200));

        // Loud operator event fired with the boundary + file.
        assert!(outcome.event_fired);
        let ev = tx.emitted.as_ref().expect("event emitted");
        assert_eq!(ev.common_point_lsn, 200);
        assert_eq!(ev.tail_to_lsn, 230);
        assert_eq!(ev.tail_lsns, 30);
        assert_eq!(ev.commit_watermark, 200);
        assert_eq!(ev.rollback_file, "/data/rollback/lsn-200-230.rbk");
        assert_eq!(ev.new_term, 8);

        // Rejoined as a replica of the new primary under the new term.
        assert_eq!(tx.rejoined, Some(("http://node-b:55055".to_string(), 8)));
        assert_eq!(
            outcome.role,
            NodeRole::Replica {
                primary_addr: "http://node-b:55055".to_string(),
                term: 8,
            }
        );

        // Critical ordering: tail is preserved BEFORE the timeline is
        // recovered, and the event fires before rejoin.
        assert_eq!(
            tx.order,
            vec!["read", "persist", "recover", "emit", "rejoin"]
        );
    }

    // ------------------------------------------------------------------
    // Full run: no divergent tail → clean rejoin, no rollback, no event
    // ------------------------------------------------------------------

    #[test]
    fn run_with_no_tail_just_rejoins() {
        // Frontier == common point: nothing diverged.
        let mut tx = FakeTransport::new(vec![]);
        let outcome =
            RollbackCoordinator::run(&request(200, 200, 200), &mut tx).expect("clean rejoin");

        assert_eq!(outcome.tail_lsns, 0);
        assert!(!outcome.rolled_back_tail());
        assert!(!outcome.event_fired, "no event when nothing is discarded");
        assert_eq!(outcome.rollback_file, None);
        assert!(tx.persisted.is_none(), "nothing persisted");
        assert!(tx.recovered_to.is_none(), "nothing recovered");
        assert!(tx.emitted.is_none(), "no operator event");
        assert_eq!(tx.rejoined, Some(("http://node-b:55055".to_string(), 8)));
        assert_eq!(tx.order, vec!["rejoin"]);
    }

    #[test]
    fn run_with_frontier_below_common_point_is_a_clean_rejoin() {
        // A node strictly behind the common point has no divergent tail;
        // it just streams forward as a replica.
        let mut tx = FakeTransport::new(vec![]);
        let outcome =
            RollbackCoordinator::run(&request(180, 200, 150), &mut tx).expect("clean rejoin");
        assert_eq!(outcome.recovered_to_lsn, 200);
        assert_eq!(outcome.tail_lsns, 0);
        assert!(!outcome.event_fired);
        assert_eq!(tx.order, vec!["rejoin"]);
    }

    // ------------------------------------------------------------------
    // Full run: refusal & abort paths
    // ------------------------------------------------------------------

    #[test]
    fn run_refuses_when_common_point_below_watermark_and_touches_nothing() {
        let mut tx = FakeTransport::new(tail(&[160, 200, 300], 7));
        let err = RollbackCoordinator::run(&request(300, 150, 200), &mut tx)
            .expect_err("must refuse to cross the watermark");
        assert!(matches!(err, RollbackError::WatermarkViolation { .. }));

        // Nothing was touched: no read, no persist, no recover, no rejoin.
        assert!(tx.persisted.is_none());
        assert!(tx.recovered_to.is_none());
        assert!(tx.emitted.is_none());
        assert!(tx.rejoined.is_none());
        assert!(tx.order.is_empty());
    }

    #[test]
    fn run_aborts_without_recovering_when_tail_cannot_be_persisted() {
        // Rollback is never silent: if the tail cannot be saved, the
        // timeline is NOT rolled back.
        let mut tx = FakeTransport::new(tail(&[210, 230], 7));
        tx.persist_should_fail = true;
        let err = RollbackCoordinator::run(&request(230, 200, 200), &mut tx)
            .expect_err("must abort when persist fails");
        assert!(matches!(err, RollbackError::TailPersistFailed { .. }));

        // Read + attempted persist happened; recover/emit/rejoin did NOT.
        assert!(tx.recovered_to.is_none(), "must not roll back the timeline");
        assert!(tx.emitted.is_none());
        assert!(tx.rejoined.is_none());
        assert_eq!(tx.order, vec!["read", "persist"]);
    }

    #[test]
    fn run_surfaces_recover_failure_after_preserving_the_tail() {
        let mut tx = FakeTransport::new(tail(&[210, 230], 7));
        tx.recover_should_fail = true;
        let err = RollbackCoordinator::run(&request(230, 200, 200), &mut tx)
            .expect_err("recover failure surfaces");
        match err {
            RollbackError::RecoverFailed { target_lsn, .. } => assert_eq!(target_lsn, 200),
            other => panic!("expected RecoverFailed, got {other:?}"),
        }

        // The tail was preserved before the failed recover, so the writes
        // are not lost; but the node did not rejoin on a half-rolled state.
        assert!(tx.persisted.is_some(), "tail preserved before recover");
        assert!(
            tx.emitted.is_none(),
            "no completion event on failed recover"
        );
        assert!(
            tx.rejoined.is_none(),
            "must not rejoin after a failed recover"
        );
        assert_eq!(tx.order, vec!["read", "persist", "recover"]);
    }

    #[test]
    fn span_lsns_counts_the_removed_range() {
        let t = DivergentTail {
            common_point_lsn: 200,
            to_lsn: 230,
            records: tail(&[210, 230], 7),
        };
        assert_eq!(t.span_lsns(), 30);
    }
}
