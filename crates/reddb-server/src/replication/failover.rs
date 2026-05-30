//! Coordinated zero-RPO failover (issue #833, PRD #819).
//!
//! Drives a *planned* primary handover so that no acknowledged write is
//! lost. The flow is the classic coordinated switchover:
//!
//! 1. **Freeze writes** on the current primary and capture its frontier
//!    LSN at the instant writes stopped. No new LSN is minted after the
//!    freeze, so the frontier is a *fixed* catch-up target.
//! 2. **Wait the target replica to the frontier** — poll the target's
//!    acknowledged (durable) frontier until it covers the frozen LSN.
//! 3. **Hand over the term** — mint `current_term + 1` and stamp it on
//!    the target, promoting it to primary.
//! 4. **Demote the old primary** to a replica that streams from the new
//!    primary under the new term.
//!
//! ## Two modes
//!
//! * [`FailoverMode::Coordinated`] is the zero-RPO path. If the target
//!   cannot reach the frontier before `catch_up_deadline`, the handover
//!   **aborts**: writes resume on the old primary and nothing is
//!   committed on the target, so the cluster keeps serving and no
//!   acknowledged write is lost (issue #833 criterion 1).
//! * [`FailoverMode::Force`] is the emergency path. It still tries to
//!   reach the frontier, but on `timeout` it completes the handover
//!   anyway, surfacing the *skipped catch-up* — the un-replicated LSN
//!   gap between the frozen frontier and the target's reached frontier
//!   (issue #833 criterion 2).
//!
//! ## Module shape
//!
//! [`FailoverCoordinator::run`] is a pure state machine. The clock and
//! the cluster mutations (freeze, resume, poll, commit) are injected
//! behind [`FailoverTransport`], so the whole flow is exercised
//! deterministically with a scripted fake — no clock, no network, no
//! engine dependency. The post-handover roles are returned in the
//! outcome ([`RoleAssignment`]) so a caller can assert that the new
//! primary advertises the new term and the old primary streams as a
//! replica (issue #833 criterion 3). Wiring the transport to the real
//! WAL frontier and the gRPC role-swap is left to the transport layer.

use std::time::Duration;

/// The replication role a node plays after a failover step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRole {
    /// Accepts writes under `term`, streams WAL to replicas.
    Primary { term: u64 },
    /// Read-only, streams WAL from `primary_addr` under `term`.
    Replica { primary_addr: String, term: u64 },
}

/// A node participating in a failover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverNode {
    /// Stable node identifier (matches the replica registry id).
    pub id: String,
    /// Dial address other nodes use to reach this node.
    pub addr: String,
    /// Region/fault-domain identifier.
    pub region: String,
}

impl FailoverNode {
    pub fn new(id: impl Into<String>, addr: impl Into<String>, region: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            addr: addr.into(),
            region: region.into(),
        }
    }
}

/// How a failover should be executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverMode {
    /// Zero-RPO coordinated handover. The target MUST reach the frozen
    /// frontier within `catch_up_deadline`; otherwise the handover
    /// aborts and writes resume on the old primary. No acknowledged
    /// write is ever lost.
    Coordinated { catch_up_deadline: Duration },
    /// Emergency handover. Tries to reach the frontier but completes
    /// within `timeout` regardless, surfacing the skipped catch-up.
    Force { timeout: Duration },
}

impl FailoverMode {
    /// Upper bound on how long the catch-up wait may run before the
    /// mode's terminal behaviour (abort vs. force) kicks in.
    fn deadline(self) -> Duration {
        match self {
            FailoverMode::Coordinated { catch_up_deadline } => catch_up_deadline,
            FailoverMode::Force { timeout } => timeout,
        }
    }

    fn is_force(self) -> bool {
        matches!(self, FailoverMode::Force { .. })
    }
}

/// A request to hand the primary role from `old_primary` to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverRequest {
    /// The node currently serving as primary.
    pub old_primary: FailoverNode,
    /// The replica being promoted.
    pub target: FailoverNode,
    /// The replication term the cluster is serving *now*. The handover
    /// mints `current_term + 1`.
    pub current_term: u64,
    /// Last known acknowledged frontier of the target (from the replica
    /// registry). Lets the coordinator take a no-wait fast path when the
    /// target is already caught up at freeze time.
    pub target_frontier_hint: u64,
    /// Coordinated vs. forced execution.
    pub mode: FailoverMode,
}

impl FailoverRequest {
    /// The term the cluster serves *after* a successful handover.
    pub fn new_term(&self) -> u64 {
        self.current_term + 1
    }
}

/// Post-handover roles of the two nodes, used to assert that the new
/// primary advertises the new term and the old primary streams as a
/// replica (issue #833 criterion 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAssignment {
    /// The promoted target — now primary under the new term.
    pub new_primary: NodeRole,
    /// The demoted old primary — now a replica of the new primary.
    pub old_primary: NodeRole,
}

impl RoleAssignment {
    fn swap(req: &FailoverRequest) -> Self {
        let new_term = req.new_term();
        Self {
            new_primary: NodeRole::Primary { term: new_term },
            old_primary: NodeRole::Replica {
                primary_addr: req.target.addr.clone(),
                term: new_term,
            },
        }
    }
}

/// The result of a completed handover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverOutcome {
    /// The term the new primary now serves.
    pub new_term: u64,
    /// The primary frontier frozen at the moment writes were paused —
    /// the catch-up target.
    pub frontier_lsn: u64,
    /// The target's acknowledged frontier at the moment the term was
    /// handed over. Equals `frontier_lsn` for a clean handover; may be
    /// below it for a forced one.
    pub reached_lsn: u64,
    /// `frontier_lsn - reached_lsn` — acknowledged-but-not-yet-replicated
    /// LSNs skipped by a forced handover. Always `0` for a clean one.
    pub skipped_lsn: u64,
    /// Whether the handover had to be forced past an un-caught-up target.
    pub forced: bool,
    /// How long the catch-up wait ran before the term was handed over.
    pub waited: Duration,
    /// Post-handover roles of the two nodes.
    pub roles: RoleAssignment,
}

impl FailoverOutcome {
    /// True when the target reached the full frontier — a true zero-RPO
    /// handover with nothing skipped.
    pub fn is_zero_rpo(&self) -> bool {
        self.skipped_lsn == 0
    }
}

/// Why a coordinated failover could not complete without losing writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailoverError {
    /// A coordinated handover aborted because the target could not reach
    /// the frozen frontier before the deadline. Writes have been resumed
    /// on the old primary; no acknowledged write was lost. Use
    /// [`FailoverMode::Force`] to hand over anyway.
    CatchUpTimedOut {
        frontier_lsn: u64,
        reached_lsn: u64,
        waited: Duration,
    },
}

impl std::fmt::Display for FailoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailoverError::CatchUpTimedOut {
                frontier_lsn,
                reached_lsn,
                waited,
            } => write!(
                f,
                "coordinated failover aborted: target reached LSN {reached_lsn} of {frontier_lsn} \
                 after {waited:?}; writes resumed on the old primary, no write lost",
            ),
        }
    }
}

impl std::error::Error for FailoverError {}

/// Cluster mutations and the clock the coordinator drives, injected so
/// the state machine stays pure and deterministically testable.
///
/// Implementors back these onto the real WAL frontier, the replica
/// registry, and the gRPC role-swap in production; tests back them onto
/// a scripted fake.
pub trait FailoverTransport {
    /// Pause writes on the current primary and return the frontier LSN
    /// (`current_lsn`) frozen at the instant writes stopped. After this
    /// returns, no new LSN is minted, so the returned value is a fixed
    /// catch-up target.
    fn freeze_primary(&mut self) -> u64;

    /// Resume writes on the old primary. Called only when a coordinated
    /// handover aborts, so the cluster keeps serving with no lost write.
    fn resume_primary(&mut self);

    /// Time elapsed since the failover began, so the coordinator can
    /// enforce the deadline without owning a clock.
    fn elapsed(&self) -> Duration;

    /// Block for one poll interval (clamped by the caller's remaining
    /// deadline in spirit), then return the target replica's current
    /// acknowledged (durable) frontier LSN.
    fn poll_target_frontier(&mut self) -> u64;

    /// Commit the role swap: stamp `new_term` on the target (promoting
    /// it to primary) and reconfigure the old primary to stream as a
    /// replica of the new primary under `new_term`.
    fn commit_handover(&mut self, new_term: u64);
}

/// The coordinated zero-RPO failover state machine.
pub struct FailoverCoordinator;

impl FailoverCoordinator {
    /// Execute the handover described by `req`, driving the cluster
    /// through `tx`.
    ///
    /// Returns `Ok(FailoverOutcome)` once the term has been handed over
    /// (cleanly, or forced past a lagging target). Returns
    /// `Err(FailoverError::CatchUpTimedOut)` only for a *coordinated*
    /// handover whose target never caught up — in which case writes have
    /// already been resumed on the old primary and nothing was committed
    /// on the target.
    pub fn run(
        req: &FailoverRequest,
        tx: &mut dyn FailoverTransport,
    ) -> Result<FailoverOutcome, FailoverError> {
        let new_term = req.new_term();
        let frontier = tx.freeze_primary();

        // Fast path: the target was already at/past the frontier when we
        // froze. Hand over immediately, no wait.
        if req.target_frontier_hint >= frontier {
            tx.commit_handover(new_term);
            return Ok(Self::clean_outcome(req, frontier, frontier, Duration::ZERO));
        }

        // Bounded wait: poll the target's live frontier until it covers
        // the frozen LSN or the deadline elapses.
        let deadline = req.mode.deadline();
        let mut reached = req.target_frontier_hint;
        while tx.elapsed() < deadline {
            reached = tx.poll_target_frontier();
            if reached >= frontier {
                let waited = tx.elapsed();
                tx.commit_handover(new_term);
                return Ok(Self::clean_outcome(req, frontier, reached, waited));
            }
        }

        // Deadline blown. A coordinated handover aborts (resume writes,
        // lose nothing); a forced one hands over anyway and surfaces the
        // skipped catch-up.
        let waited = tx.elapsed();
        if req.mode.is_force() {
            tx.commit_handover(new_term);
            Ok(FailoverOutcome {
                new_term,
                frontier_lsn: frontier,
                reached_lsn: reached,
                skipped_lsn: frontier.saturating_sub(reached),
                forced: true,
                waited,
                roles: RoleAssignment::swap(req),
            })
        } else {
            tx.resume_primary();
            Err(FailoverError::CatchUpTimedOut {
                frontier_lsn: frontier,
                reached_lsn: reached,
                waited,
            })
        }
    }

    fn clean_outcome(
        req: &FailoverRequest,
        frontier: u64,
        reached: u64,
        waited: Duration,
    ) -> FailoverOutcome {
        FailoverOutcome {
            new_term: req.new_term(),
            frontier_lsn: frontier,
            reached_lsn: reached,
            skipped_lsn: 0,
            forced: false,
            waited,
            roles: RoleAssignment::swap(req),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted transport: a fixed frozen frontier and a queue of target
    /// frontier readings consumed one per poll. `tick` advances the fake
    /// clock by a fixed step on every poll so deadlines are exercised
    /// deterministically.
    struct FakeTransport {
        frontier: u64,
        readings: std::collections::VecDeque<u64>,
        /// Last reading repeated once the script is exhausted (replica
        /// stuck behind).
        stuck_at: u64,
        elapsed: Duration,
        tick: Duration,
        froze: bool,
        resumed: bool,
        committed_term: Option<u64>,
    }

    impl FakeTransport {
        fn new(frontier: u64, readings: Vec<u64>, tick: Duration) -> Self {
            let stuck_at = readings.last().copied().unwrap_or(0);
            Self {
                frontier,
                readings: readings.into(),
                stuck_at,
                elapsed: Duration::ZERO,
                tick,
                froze: false,
                resumed: false,
                committed_term: None,
            }
        }
    }

    impl FailoverTransport for FakeTransport {
        fn freeze_primary(&mut self) -> u64 {
            self.froze = true;
            self.frontier
        }
        fn resume_primary(&mut self) {
            self.resumed = true;
        }
        fn elapsed(&self) -> Duration {
            self.elapsed
        }
        fn poll_target_frontier(&mut self) -> u64 {
            self.elapsed += self.tick;
            self.readings.pop_front().unwrap_or(self.stuck_at)
        }
        fn commit_handover(&mut self, new_term: u64) {
            self.committed_term = Some(new_term);
        }
    }

    fn request(mode: FailoverMode, hint: u64) -> FailoverRequest {
        FailoverRequest {
            old_primary: FailoverNode::new("n1", "http://n1:50051", "us-east"),
            target: FailoverNode::new("n2", "http://n2:50051", "us-west"),
            current_term: 4,
            target_frontier_hint: hint,
            mode,
        }
    }

    #[test]
    fn fast_path_hands_over_without_waiting_when_target_already_caught_up() {
        let mut tx = FakeTransport::new(100, vec![], Duration::from_millis(10));
        let req = request(
            FailoverMode::Coordinated {
                catch_up_deadline: Duration::from_secs(5),
            },
            100,
        );

        let outcome = FailoverCoordinator::run(&req, &mut tx).expect("clean handover");

        assert!(tx.froze, "writes must be paused");
        assert_eq!(tx.committed_term, Some(5), "new term handed over");
        assert!(!tx.resumed, "no abort on a clean handover");
        assert_eq!(outcome.waited, Duration::ZERO, "fast path does not wait");
        assert!(outcome.is_zero_rpo());
        assert_eq!(outcome.skipped_lsn, 0);
    }

    #[test]
    fn coordinated_waits_then_hands_over_when_target_catches_up() {
        // Target climbs 60 -> 80 -> 100, reaching the frontier on poll 3.
        let mut tx = FakeTransport::new(100, vec![60, 80, 100], Duration::from_millis(10));
        let req = request(
            FailoverMode::Coordinated {
                catch_up_deadline: Duration::from_secs(5),
            },
            50,
        );

        let outcome = FailoverCoordinator::run(&req, &mut tx).expect("clean handover");

        assert_eq!(tx.committed_term, Some(5));
        assert!(!tx.resumed);
        assert_eq!(outcome.new_term, 5);
        assert_eq!(outcome.frontier_lsn, 100);
        assert_eq!(outcome.reached_lsn, 100);
        assert!(outcome.is_zero_rpo(), "no write lost in a clean handover");
        assert_eq!(outcome.waited, Duration::from_millis(30));
        assert_eq!(
            outcome.roles.new_primary,
            NodeRole::Primary { term: 5 },
            "new primary advertises the new term",
        );
        assert_eq!(
            outcome.roles.old_primary,
            NodeRole::Replica {
                primary_addr: "http://n2:50051".to_string(),
                term: 5,
            },
            "old primary streams as a replica of the new primary",
        );
    }

    #[test]
    fn coordinated_aborts_and_resumes_when_target_never_catches_up() {
        // Target stalls at 70, never reaching the frontier of 100 before
        // the 50ms deadline (5 polls at 10ms).
        let mut tx = FakeTransport::new(100, vec![60, 65, 70], Duration::from_millis(10));
        let req = request(
            FailoverMode::Coordinated {
                catch_up_deadline: Duration::from_millis(50),
            },
            50,
        );

        let err = FailoverCoordinator::run(&req, &mut tx).expect_err("must abort");

        assert!(tx.resumed, "writes must resume on the old primary");
        assert_eq!(tx.committed_term, None, "no term handed over on abort");
        match err {
            FailoverError::CatchUpTimedOut {
                frontier_lsn,
                reached_lsn,
                ..
            } => {
                assert_eq!(frontier_lsn, 100);
                assert_eq!(reached_lsn, 70);
            }
        }
    }

    #[test]
    fn force_completes_within_timeout_surfacing_skipped_catch_up() {
        // Target stalls at 70; FORCE hands over anyway after the timeout.
        let mut tx = FakeTransport::new(100, vec![60, 65, 70], Duration::from_millis(10));
        let req = request(
            FailoverMode::Force {
                timeout: Duration::from_millis(50),
            },
            50,
        );

        let outcome = FailoverCoordinator::run(&req, &mut tx).expect("forced handover");

        assert!(!tx.resumed, "forced handover does not abort");
        assert_eq!(tx.committed_term, Some(5), "term handed over under force");
        assert!(outcome.forced);
        assert_eq!(outcome.frontier_lsn, 100);
        assert_eq!(outcome.reached_lsn, 70);
        assert_eq!(outcome.skipped_lsn, 30, "skipped catch-up surfaced");
        assert!(!outcome.is_zero_rpo());
        assert!(
            outcome.waited <= Duration::from_millis(60),
            "completes within the timeout window",
        );
        assert_eq!(outcome.roles.new_primary, NodeRole::Primary { term: 5 });
    }

    #[test]
    fn force_still_takes_fast_path_when_target_already_caught_up() {
        let mut tx = FakeTransport::new(100, vec![], Duration::from_millis(10));
        let req = request(
            FailoverMode::Force {
                timeout: Duration::from_millis(50),
            },
            120,
        );

        let outcome = FailoverCoordinator::run(&req, &mut tx).expect("clean forced handover");

        assert!(!outcome.forced, "no force needed when already caught up");
        assert_eq!(outcome.skipped_lsn, 0);
        assert!(outcome.is_zero_rpo());
    }

    #[test]
    fn force_that_catches_up_in_time_skips_nothing() {
        let mut tx = FakeTransport::new(100, vec![90, 100], Duration::from_millis(10));
        let req = request(
            FailoverMode::Force {
                timeout: Duration::from_secs(5),
            },
            50,
        );

        let outcome = FailoverCoordinator::run(&req, &mut tx).expect("forced handover catches up");

        assert!(!outcome.forced);
        assert_eq!(outcome.skipped_lsn, 0);
        assert!(outcome.is_zero_rpo());
    }
}
