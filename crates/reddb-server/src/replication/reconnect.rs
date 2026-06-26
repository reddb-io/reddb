//! Primary↔replica reconnect telemetry (issue #1243, PRD #1237 Phase B).
//!
//! A replica drives a long-lived gRPC pull loop against its primary
//! (`run_replica_loop`). When that link drops — a failed `pull_wal_records`
//! or the initial `connect` retry — the loop persists the `connecting`
//! health state and keeps retrying. When a pull succeeds again the loop
//! persists `healthy`. A **reconnect** is exactly that transition: a link
//! that was up, went down, and came back up.
//!
//! This module records that signal as a monotonic counter so red-ui can
//! explain instability ("this replica has reconnected 14 times in the last
//! hour") instead of showing only a last-error snapshot. It is the
//! reconnect-counter producer/consumer slice of the operational telemetry
//! substrate (ADR 0060): measurement here, export through `/metrics`
//! (`reddb_replication_reconnects_total`) and the red-ui status read model.
//!
//! # What is *not* counted
//!
//! - The **initial** connect on startup. A replica coming up for the first
//!   time is not "reconnecting"; the counter only moves once the link has
//!   been healthy at least once and then recovers from a drop.
//! - Apply-side failures (`apply_error`, `divergence`, `relay_error`,
//!   `ack_error`, …). Those are not link drops — the gRPC stream is still
//!   up — and have their own counters
//!   (`reddb_replica_apply_errors_total`). Treating them as drops would
//!   inflate the reconnect count, so only the `connecting` state marks the
//!   link down.
//!
//! # Privacy (ADR 0060 §5)
//!
//! Reconnect telemetry stores **no** endpoint, URL, credential, or
//! authorization material — only a monotonic count and the bounded
//! transition state. The exported series carries the node's own stable
//! `replica_id` as its single dimension, never the primary's address.
//!
//! # Reset / restart behavior
//!
//! The counter is an in-memory `AtomicU64` scoped to the process lifetime.
//! It starts at `0` on every boot and is **not** persisted across restarts
//! — a process restart resets it to `0`, exactly like the sibling
//! `reddb_replication_full_resync_total` / `_partial_resync_total` counters.
//! Prometheus treats a counter that drops to `0` as a reset (via the
//! process-start timestamp), so `rate()`/`increase()` stay correct across a
//! restart. red-ui reads the live value; it does not assume monotonicity
//! across a process boundary.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The replica's view of the link to its primary, as projected from the
/// health states the pull loop persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkState {
    /// No health state observed yet (pre-connect).
    Unknown,
    /// The link has been healthy at least once and is currently up.
    Up,
    /// The link was up before and is currently down (retrying connect).
    Down,
}

#[derive(Debug)]
struct LinkTracker {
    state: LinkState,
}

/// Process-lifetime reconnect telemetry for the local replica's link to its
/// primary. Lives on the runtime beside [`super::logical::ReplicaApplyMetrics`]
/// and is driven from the single health-persist chokepoint in the replica
/// loop, so every link-state transition is observed exactly once.
#[derive(Debug)]
pub struct ReplicaLinkMetrics {
    reconnects_total: AtomicU64,
    tracker: Mutex<LinkTracker>,
}

impl Default for ReplicaLinkMetrics {
    fn default() -> Self {
        Self {
            reconnects_total: AtomicU64::new(0),
            tracker: Mutex::new(LinkTracker {
                state: LinkState::Unknown,
            }),
        }
    }
}

impl ReplicaLinkMetrics {
    /// Observe a persisted replica health `state` string and advance the
    /// reconnect counter when the link transitions from down back to up.
    ///
    /// Only `"healthy"` counts as the link being up and only `"connecting"`
    /// counts as the link being down; every other state (apply errors,
    /// rebootstrap phases, rejoining, …) is neither a clean up nor a link
    /// drop and leaves the tracked state unchanged, so it can neither arm
    /// nor fire a reconnect.
    pub fn observe_state(&self, state: &str) {
        let mut tracker = match self.tracker.lock() {
            Ok(guard) => guard,
            // A poisoned lock means a prior observer panicked mid-update.
            // Reconnect telemetry must never take down the loop that feeds
            // it, so recover the guard and carry on; the worst case is a
            // single mis-attributed transition.
            Err(poisoned) => poisoned.into_inner(),
        };
        match state {
            "healthy" => {
                if tracker.state == LinkState::Down {
                    // down -> up after having been up before: a reconnect.
                    self.reconnects_total.fetch_add(1, Ordering::Relaxed);
                }
                tracker.state = LinkState::Up;
            }
            // Only arm a reconnect once the link has been up at least once;
            // the initial connect (state still `Unknown`) must not be counted.
            "connecting" if tracker.state == LinkState::Up => {
                tracker.state = LinkState::Down;
            }
            _ => {}
        }
    }

    /// Total reconnects observed since process start. Monotonic within a
    /// process; resets to `0` on restart (see module docs).
    pub fn reconnects_total(&self) -> u64 {
        self.reconnects_total.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_connect_is_not_a_reconnect() {
        let metrics = ReplicaLinkMetrics::default();
        // Startup: the loop persists `connecting` while it waits for the
        // primary, then `healthy` once the first pull lands.
        metrics.observe_state("connecting");
        metrics.observe_state("healthy");
        assert_eq!(metrics.reconnects_total(), 0);
    }

    #[test]
    fn drop_and_restore_counts_one_reconnect() {
        let metrics = ReplicaLinkMetrics::default();
        metrics.observe_state("connecting");
        metrics.observe_state("healthy");
        // Link drops (a failed pull) then restores.
        metrics.observe_state("connecting");
        metrics.observe_state("healthy");
        assert_eq!(metrics.reconnects_total(), 1);
    }

    #[test]
    fn repeated_connecting_during_one_outage_counts_once() {
        let metrics = ReplicaLinkMetrics::default();
        metrics.observe_state("healthy");
        // A multi-poll outage persists `connecting` several times.
        metrics.observe_state("connecting");
        metrics.observe_state("connecting");
        metrics.observe_state("connecting");
        metrics.observe_state("healthy");
        assert_eq!(metrics.reconnects_total(), 1);
    }

    #[test]
    fn multiple_outages_accumulate() {
        let metrics = ReplicaLinkMetrics::default();
        metrics.observe_state("healthy");
        for _ in 0..5 {
            metrics.observe_state("connecting");
            metrics.observe_state("healthy");
        }
        assert_eq!(metrics.reconnects_total(), 5);
    }

    #[test]
    fn apply_errors_are_not_reconnects() {
        let metrics = ReplicaLinkMetrics::default();
        metrics.observe_state("healthy");
        // The stream is still up; these are apply-side, not link drops.
        metrics.observe_state("apply_error");
        metrics.observe_state("divergence");
        metrics.observe_state("relay_error");
        metrics.observe_state("ack_error");
        metrics.observe_state("healthy");
        assert_eq!(metrics.reconnects_total(), 0);
    }

    #[test]
    fn apply_error_during_outage_does_not_disarm_reconnect() {
        let metrics = ReplicaLinkMetrics::default();
        metrics.observe_state("healthy");
        metrics.observe_state("connecting");
        // An interleaved non-link state must not clear the armed drop.
        metrics.observe_state("apply_error");
        metrics.observe_state("healthy");
        assert_eq!(metrics.reconnects_total(), 1);
    }
}
