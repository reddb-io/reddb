//! Write-admission flow control keyed on in-quorum replica lag (issue #826).
//!
//! The primary streams WAL to every connected replica, but only some of
//! those replicas count toward the configured commit quorum. When a
//! *quorum member* falls behind, the primary should slow incoming writes
//! so the lagging member can catch up — otherwise sync/quorum commits
//! stall and the lag compounds. Replicas that are pure read scale-out
//! (async, not in the quorum) must never exert this back-pressure: read
//! fan-out should not be able to throttle write throughput.
//!
//! `FlowController` implements that policy as a ticket-based admission
//! gate. It watches the max lag across *in-quorum* replicas against a
//! soft target (in LSN records):
//!
//! * lag `<=` soft target → tickets flow, writes admitted.
//! * lag `>`  soft target → throttle engaged, admission tickets denied
//!   until the quorum member recovers below the target.
//!
//! A soft target of `0` disables the feature entirely (the default), so
//! standalone and async-commit deployments are unaffected. The decision
//! mirrors the engine-managed graceful-pause precedent in
//! [`crate::runtime::write_gate`] (issue #519 archive-lag auto-pause):
//! an independent, automatically-engaging/releasing gate that the
//! operator's manual read-only pin never stomps.
//!
//! In-quorum membership is derived from the active [`QuorumConfig`]:
//!
//! * [`QuorumMode::Async`] — no replica is synchronous, so *nothing* is
//!   in-quorum and the controller never throttles.
//! * [`QuorumMode::Sync`] — every connected replica is a candidate for
//!   the synchronous quorum and counts toward the lag signal.
//! * [`QuorumMode::Regions`] — only replicas whose declared region is in
//!   the required set count; replicas in other regions (or with no
//!   region) are async read-replicas and are excluded.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::primary::ReplicaState;
use super::quorum::{QuorumConfig, QuorumMode};

/// Outcome of a write-admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A ticket was issued — the write may proceed.
    Granted,
    /// Throttle engaged — an in-quorum replica's lag exceeds the soft
    /// target. The caller must not proceed with the write.
    Throttled,
}

impl Admission {
    pub fn is_granted(self) -> bool {
        matches!(self, Admission::Granted)
    }
}

/// Is `replica` a member of the commit quorum under `quorum`?
///
/// Async read-replicas (not in the quorum) return `false` and are
/// therefore excluded from the flow-control lag signal.
pub fn is_in_quorum(replica: &ReplicaState, quorum: &QuorumConfig) -> bool {
    match &quorum.mode {
        // Async-commit: nobody is synchronous, so no replica gates writes.
        QuorumMode::Async => false,
        // Sync(n): every connected replica is a quorum candidate.
        QuorumMode::Sync { .. } => true,
        // Regions: only replicas in a required region are in-quorum.
        QuorumMode::Regions { required } => replica
            .region
            .as_deref()
            .map(|region| required.contains(region))
            .unwrap_or(false),
    }
}

/// Max lag in LSN records across the in-quorum replicas, measured as the
/// distance from the primary's current LSN to each replica's last acked
/// LSN. Async read-replicas are excluded. Returns `0` when no replica is
/// in-quorum (so the controller never throttles on read scale-out alone).
pub fn in_quorum_max_lag_lsn(
    replicas: &[ReplicaState],
    primary_lsn: u64,
    quorum: &QuorumConfig,
) -> u64 {
    replicas
        .iter()
        .filter(|replica| is_in_quorum(replica, quorum))
        .map(|replica| primary_lsn.saturating_sub(replica.last_acked_lsn))
        .max()
        .unwrap_or(0)
}

/// Ticket-based write-admission flow controller.
///
/// Holds the soft-target policy and the live throttle state. Cheap to
/// share: every field is a single atomic plus the immutable quorum
/// config, so [`Self::try_admit`] on the write hot path is a couple of
/// relaxed loads.
#[derive(Debug)]
pub struct FlowController {
    /// Soft target lag in LSN records. `0` disables throttling.
    soft_target_lsn: AtomicU64,
    /// Whether the throttle is currently engaged.
    throttled: AtomicBool,
    /// Most recent observed max in-quorum lag (for metrics).
    observed_lag_lsn: AtomicU64,
    /// Active quorum shape — decides which replicas count as in-quorum.
    quorum: QuorumConfig,
}

impl FlowController {
    /// A disabled controller (soft target `0`): never throttles.
    pub fn disabled() -> Self {
        Self::new(0, QuorumConfig::async_commit())
    }

    /// Build a controller with an explicit soft target and quorum shape.
    pub fn new(soft_target_lsn: u64, quorum: QuorumConfig) -> Self {
        Self {
            soft_target_lsn: AtomicU64::new(soft_target_lsn),
            throttled: AtomicBool::new(false),
            observed_lag_lsn: AtomicU64::new(0),
            quorum,
        }
    }

    /// Install (or change) the soft target at runtime. `0` disables the
    /// feature and immediately releases any active throttle.
    pub fn configure_soft_target(&self, soft_target_lsn: u64) {
        self.soft_target_lsn
            .store(soft_target_lsn, Ordering::Release);
        if soft_target_lsn == 0 {
            self.throttled.store(false, Ordering::Release);
        }
    }

    /// Soft target in LSN records. `0` means disabled.
    pub fn soft_target_lsn(&self) -> u64 {
        self.soft_target_lsn.load(Ordering::Acquire)
    }

    /// Whether flow control is enabled (soft target `> 0`).
    pub fn is_enabled(&self) -> bool {
        self.soft_target_lsn() > 0
    }

    /// Whether the throttle is currently engaged.
    pub fn is_throttled(&self) -> bool {
        self.throttled.load(Ordering::Acquire)
    }

    /// Most recent observed max in-quorum replica lag in LSN records.
    pub fn observed_lag_lsn(&self) -> u64 {
        self.observed_lag_lsn.load(Ordering::Acquire)
    }

    /// Re-evaluate the throttle from a replica snapshot.
    ///
    /// Computes the max lag across in-quorum replicas (async read-replicas
    /// excluded) and engages the throttle when it exceeds the soft target,
    /// releasing it when the quorum member recovers to/below the target.
    /// Returns the resulting `throttled` state.
    pub fn observe(&self, replicas: &[ReplicaState], primary_lsn: u64) -> bool {
        let soft_target = self.soft_target_lsn();
        let lag = in_quorum_max_lag_lsn(replicas, primary_lsn, &self.quorum);
        self.observed_lag_lsn.store(lag, Ordering::Release);
        // Disabled (soft target 0) can never throttle.
        let throttled = soft_target > 0 && lag > soft_target;
        self.throttled.store(throttled, Ordering::Release);
        throttled
    }

    /// Request a write-admission ticket. `Granted` unless the throttle is
    /// engaged. The check is a single relaxed load on the hot path.
    pub fn try_admit(&self) -> Admission {
        if self.is_throttled() {
            Admission::Throttled
        } else {
            Admission::Granted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica(id: &str, region: Option<&str>, last_acked_lsn: u64) -> ReplicaState {
        ReplicaState {
            id: id.to_string(),
            last_acked_lsn,
            last_sent_lsn: last_acked_lsn,
            last_durable_lsn: last_acked_lsn,
            apply_error_count: 0,
            divergence_count: 0,
            connected_at_unix_ms: 0,
            last_seen_at_unix_ms: 0,
            region: region.map(String::from),
            rebootstrapping: false,
        }
    }

    #[test]
    fn async_mode_classifies_no_replica_in_quorum() {
        let q = QuorumConfig::async_commit();
        assert!(!is_in_quorum(&replica("r1", Some("us"), 0), &q));
    }

    #[test]
    fn sync_mode_classifies_every_replica_in_quorum() {
        let q = QuorumConfig::sync(2);
        assert!(is_in_quorum(&replica("r1", None, 0), &q));
        assert!(is_in_quorum(&replica("r2", Some("eu"), 0), &q));
    }

    #[test]
    fn regions_mode_classifies_only_required_regions_in_quorum() {
        let q = QuorumConfig::regions(["us", "eu"]);
        assert!(is_in_quorum(&replica("r1", Some("us"), 0), &q));
        assert!(is_in_quorum(&replica("r2", Some("eu"), 0), &q));
        // async read-replica in a non-required region — excluded.
        assert!(!is_in_quorum(&replica("r3", Some("ap"), 0), &q));
        // no declared region — excluded.
        assert!(!is_in_quorum(&replica("r4", None, 0), &q));
    }

    #[test]
    fn disabled_controller_never_throttles() {
        let fc = FlowController::disabled();
        let replicas = vec![replica("r1", Some("us"), 0)];
        // Huge lag, but soft target 0 → no throttle.
        assert!(!fc.observe(&replicas, 1_000_000));
        assert!(!fc.is_throttled());
        assert_eq!(fc.try_admit(), Admission::Granted);
    }

    #[test]
    fn engages_when_in_quorum_replica_exceeds_soft_target() {
        let fc = FlowController::new(100, QuorumConfig::sync(1));
        // primary at 500, replica acked 350 → lag 150 > 100.
        let replicas = vec![replica("r1", Some("us"), 350)];
        assert!(fc.observe(&replicas, 500));
        assert!(fc.is_throttled());
        assert_eq!(fc.observed_lag_lsn(), 150);
        assert_eq!(fc.try_admit(), Admission::Throttled);
    }

    #[test]
    fn releases_when_in_quorum_replica_recovers() {
        let fc = FlowController::new(100, QuorumConfig::sync(1));
        let lagging = vec![replica("r1", Some("us"), 350)];
        assert!(fc.observe(&lagging, 500));
        assert_eq!(fc.try_admit(), Admission::Throttled);

        // Replica catches up to within the soft target (lag 50 <= 100).
        let recovered = vec![replica("r1", Some("us"), 450)];
        assert!(!fc.observe(&recovered, 500));
        assert!(!fc.is_throttled());
        assert_eq!(fc.observed_lag_lsn(), 50);
        assert_eq!(fc.try_admit(), Admission::Granted);
    }

    #[test]
    fn at_soft_target_boundary_does_not_throttle() {
        let fc = FlowController::new(100, QuorumConfig::sync(1));
        // lag exactly == soft target → not throttled (strictly greater).
        let replicas = vec![replica("r1", Some("us"), 400)];
        assert!(!fc.observe(&replicas, 500));
        assert!(!fc.is_throttled());
    }

    #[test]
    fn async_read_replica_lag_never_engages_throttling() {
        // Regions quorum requires "us". An async read-replica in "ap"
        // lags massively, but the in-quorum "us" replica is caught up.
        let fc = FlowController::new(100, QuorumConfig::regions(["us"]));
        let replicas = vec![
            replica("in-quorum-us", Some("us"), 500), // caught up
            replica("async-ap", Some("ap"), 0),       // lag 500, excluded
        ];
        assert!(!fc.observe(&replicas, 500));
        assert!(!fc.is_throttled());
        // The lag signal reflects only the in-quorum replica (0), not the
        // async read-replica's 500-record lag.
        assert_eq!(fc.observed_lag_lsn(), 0);
        assert_eq!(fc.try_admit(), Admission::Granted);
    }

    #[test]
    fn in_quorum_replica_still_throttles_with_async_replica_present() {
        // Same shape, but now the in-quorum "us" replica lags past target
        // while the async "ap" replica is caught up — must still throttle.
        let fc = FlowController::new(100, QuorumConfig::regions(["us"]));
        let replicas = vec![
            replica("in-quorum-us", Some("us"), 300), // lag 200 > 100
            replica("async-ap", Some("ap"), 500),     // caught up, excluded
        ];
        assert!(fc.observe(&replicas, 500));
        assert!(fc.is_throttled());
        assert_eq!(fc.observed_lag_lsn(), 200);
    }

    #[test]
    fn configure_soft_target_zero_releases_throttle() {
        let fc = FlowController::new(100, QuorumConfig::sync(1));
        assert!(fc.observe(&[replica("r1", Some("us"), 0)], 500));
        assert!(fc.is_throttled());
        // Operator disables flow control — throttle releases immediately.
        fc.configure_soft_target(0);
        assert!(!fc.is_enabled());
        assert!(!fc.is_throttled());
        assert_eq!(fc.try_admit(), Admission::Granted);
    }

    #[test]
    fn no_in_quorum_replicas_never_throttles() {
        // Sync quorum configured but only async (region-excluded under a
        // regions quorum) — here Sync counts all, so use regions with no
        // matching replica to prove "no in-quorum members → lag 0".
        let fc = FlowController::new(10, QuorumConfig::regions(["us"]));
        let replicas = vec![replica("ap-only", Some("ap"), 0)];
        assert!(!fc.observe(&replicas, 1_000));
        assert_eq!(fc.observed_lag_lsn(), 0);
        assert!(!fc.is_throttled());
    }
}
