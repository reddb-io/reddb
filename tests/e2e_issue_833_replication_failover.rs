//! Issue #833 — coordinated zero-RPO FAILOVER command.
//!
//! Demonstrates an end-to-end handover against a modelled two-node
//! cluster: writes pause on the primary, the target replica catches up
//! to the frozen frontier, the term is handed over, and the old primary
//! is demoted to a replica. The forced variant completes within its
//! timeout even when the target is slightly behind, surfacing the
//! skipped catch-up.

use std::collections::VecDeque;
use std::time::Duration;

use reddb::replication::failover::{
    FailoverCoordinator, FailoverError, FailoverMode, FailoverNode, FailoverRequest,
    FailoverTransport, NodeRole,
};

/// A modelled two-node cluster the failover coordinator drives. Tracks
/// the live role of each node and the primary's WAL frontier so the test
/// can assert the post-handover topology, not just the returned outcome.
struct ClusterModel {
    primary_id: String,
    target_id: String,
    /// Primary frontier (current_lsn). Frozen on `freeze_primary`.
    primary_frontier: u64,
    /// Live role of each node, keyed by id.
    primary_role: NodeRole,
    target_role: NodeRole,
    /// Whether the primary is currently accepting writes.
    writes_paused: bool,
    /// Scripted target frontier readings, one consumed per poll.
    readings: VecDeque<u64>,
    stuck_at: u64,
    elapsed: Duration,
    tick: Duration,
}

impl ClusterModel {
    fn new(term: u64, primary_frontier: u64, readings: Vec<u64>) -> Self {
        let stuck_at = readings.last().copied().unwrap_or(0);
        Self {
            primary_id: "node-a".to_string(),
            target_id: "node-b".to_string(),
            primary_frontier,
            primary_role: NodeRole::Primary { term },
            target_role: NodeRole::Replica {
                primary_addr: "http://node-a:50051".to_string(),
                term,
            },
            writes_paused: false,
            readings: readings.into(),
            stuck_at,
            elapsed: Duration::ZERO,
            tick: Duration::from_millis(10),
        }
    }

    fn request(&self, mode: FailoverMode, current_term: u64, hint: u64) -> FailoverRequest {
        FailoverRequest {
            old_primary: FailoverNode::new(&self.primary_id, "http://node-a:50051", "us-east"),
            target: FailoverNode::new(&self.target_id, "http://node-b:50051", "us-west"),
            current_term,
            target_frontier_hint: hint,
            timeline_history: reddb::TimelineHistory::new(10),
            mode,
        }
    }
}

impl FailoverTransport for ClusterModel {
    fn freeze_primary(&mut self) -> u64 {
        self.writes_paused = true;
        self.primary_frontier
    }

    fn resume_primary(&mut self) {
        self.writes_paused = false;
    }

    fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn poll_target_frontier(&mut self) -> u64 {
        self.elapsed += self.tick;
        self.readings.pop_front().unwrap_or(self.stuck_at)
    }

    fn commit_handover(&mut self, new_term: u64) {
        // Promote the target to primary and demote the old primary to a
        // replica streaming from the new primary, both under the new term.
        self.target_role = NodeRole::Primary { term: new_term };
        self.primary_role = NodeRole::Replica {
            primary_addr: "http://node-b:50051".to_string(),
            term: new_term,
        };
    }
}

#[test]
fn clean_handover_swaps_roles_with_no_lost_write() {
    // Target climbs to the frozen frontier of 200 across three polls.
    let mut cluster = ClusterModel::new(7, 200, vec![150, 180, 200]);
    let req = cluster.request(
        FailoverMode::Coordinated {
            catch_up_deadline: Duration::from_secs(5),
        },
        7,
        120,
    );

    let outcome = FailoverCoordinator::run(&req, &mut cluster).expect("clean handover succeeds");

    // Zero RPO: the target reached the full frontier, nothing skipped.
    assert!(outcome.is_zero_rpo());
    assert_eq!(outcome.skipped_lsn, 0);
    assert_eq!(outcome.new_term, 8);
    assert_eq!(outcome.frontier_lsn, 200);
    assert_eq!(outcome.reached_lsn, 200);
    assert_eq!(
        outcome.timeline_history.current(),
        Some(reddb::TimelineId(2))
    );
    assert_eq!(
        outcome.timeline_history.ancestor_lsn(reddb::TimelineId(2)),
        Some(200)
    );

    // Roles swapped: new primary advertises the new term, old primary is
    // now a replica of it.
    assert_eq!(cluster.target_role, NodeRole::Primary { term: 8 });
    assert_eq!(
        cluster.primary_role,
        NodeRole::Replica {
            primary_addr: "http://node-b:50051".to_string(),
            term: 8,
        },
    );
    assert!(
        cluster.writes_paused,
        "writes stay paused on the demoted old primary after a clean handover",
    );
}

#[test]
fn coordinated_handover_aborts_without_losing_writes_when_target_lags() {
    // Target stalls at 160, never reaching the frontier of 200 before the
    // 40ms deadline elapses.
    let mut cluster = ClusterModel::new(7, 200, vec![140, 150, 160]);
    let req = cluster.request(
        FailoverMode::Coordinated {
            catch_up_deadline: Duration::from_millis(40),
        },
        7,
        120,
    );

    let err =
        FailoverCoordinator::run(&req, &mut cluster).expect_err("must abort, not lose writes");

    match err {
        FailoverError::CatchUpTimedOut {
            frontier_lsn,
            reached_lsn,
            ..
        } => {
            assert_eq!(frontier_lsn, 200);
            assert_eq!(reached_lsn, 160);
        }
        FailoverError::TimelineHistory(err) => panic!("unexpected timeline error: {err}"),
    }

    // No handover happened: roles unchanged and writes resumed on the
    // original primary so no acknowledged write is lost.
    assert_eq!(cluster.primary_role, NodeRole::Primary { term: 7 });
    assert!(matches!(cluster.target_role, NodeRole::Replica { .. }));
    assert!(!cluster.writes_paused, "writes resume on the old primary");
}

#[test]
fn forced_handover_completes_within_timeout_surfacing_skipped_catch_up() {
    // Emergency handover: the target stalls at 170 but FORCE completes
    // anyway within the 40ms timeout.
    let mut cluster = ClusterModel::new(7, 200, vec![150, 160, 170]);
    let req = cluster.request(
        FailoverMode::Force {
            timeout: Duration::from_millis(40),
        },
        7,
        120,
    );

    let outcome = FailoverCoordinator::run(&req, &mut cluster).expect("forced handover succeeds");

    assert!(outcome.forced);
    assert_eq!(outcome.frontier_lsn, 200);
    assert_eq!(outcome.reached_lsn, 170);
    assert_eq!(outcome.skipped_lsn, 30, "skipped catch-up surfaced");
    assert!(!outcome.is_zero_rpo());
    assert_eq!(
        outcome.timeline_history.ancestor_lsn(reddb::TimelineId(2)),
        Some(170),
        "forced timeline forks where the target actually reached"
    );
    assert!(
        outcome.waited <= Duration::from_millis(50),
        "forced handover completes within the timeout window",
    );

    // Roles still swap under the new term despite the forced gap.
    assert_eq!(cluster.target_role, NodeRole::Primary { term: 8 });
    assert_eq!(
        cluster.primary_role,
        NodeRole::Replica {
            primary_addr: "http://node-b:50051".to_string(),
            term: 8,
        },
    );
}
