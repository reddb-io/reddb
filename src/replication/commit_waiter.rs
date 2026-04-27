//! Synchronous commit waiter (PLAN.md Phase 11.4 — `ack_n`).
//!
//! Bridges the primary's commit path with replica ACKs. The commit
//! caller picks a `target_lsn` (the LSN it just made durable
//! locally) and asks the waiter "block until at least N replicas
//! have ack'd this LSN, or the timeout expires." Replica ACK RPCs
//! call `record_replica_ack` which signals every waiter whose
//! threshold is now met.
//!
//! ## Thread safety
//!
//! The waiter uses a `Mutex<State>` + `Condvar` so the `await_acks`
//! call blocks the caller's thread without spinning. Acks bump a
//! per-replica `last_durable_lsn` map and broadcast on the condvar.
//! Waiters wake, recompute the count of replicas at or past their
//! target, and either return `Ok(count)` or re-wait.
//!
//! ## Why this is just the foundation
//!
//! The actual write commit path doesn't yet call `await_acks` —
//! wiring it in touches every public mutation surface and changes
//! latency characteristics across the board. This module ships the
//! primitive + the ack registry so the wiring change can land as
//! one focused PR per surface (HTTP, gRPC, wire protocol) rather
//! than a single massive diff.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
struct State {
    /// Per-replica durable LSN. Updated by `record_replica_ack`.
    /// Replicas absent from this map are treated as having durable
    /// LSN 0 (haven't acked anything yet).
    durable_lsn: HashMap<String, u64>,
}

/// Outcome counters for /metrics. PLAN.md Phase 11.4 — operators
/// alert on `timed_out` rising (commit policy is too tight or
/// replicas are stalled) and watch `last_wait_micros` for the p95.
#[derive(Debug, Default)]
pub struct CommitWaiterMetrics {
    pub reached_total: AtomicU64,
    pub timed_out_total: AtomicU64,
    pub not_required_total: AtomicU64,
    /// Wall-clock micros of the most recent `Reached` or `TimedOut`
    /// wait. Gauge, not histogram — keeps the no-extra-deps line.
    pub last_wait_micros: AtomicU64,
}

#[derive(Debug)]
pub struct CommitWaiter {
    state: Mutex<State>,
    cond: Condvar,
    metrics: CommitWaiterMetrics,
}

impl Default for CommitWaiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            metrics: CommitWaiterMetrics::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitOutcome {
    /// At least `required` replicas reached `target_lsn` before the
    /// deadline. The returned count is the number observed at the
    /// moment we unblocked (may exceed `required` if many replicas
    /// were already ahead).
    Reached(u32),
    /// The deadline expired with fewer than `required` replicas at
    /// or past `target_lsn`. The count we observed is included so
    /// the caller can log how close we got.
    TimedOut { observed: u32, required: u32 },
    /// `required == 0` — degenerate case, returns immediately. No
    /// replica state is consulted.
    NotRequired,
}

impl CommitWaiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replica reports it has durably persisted up to `lsn`.
    /// Idempotent: only advances forward. Wakes every waiter so they
    /// can recheck their threshold.
    pub fn record_replica_ack(&self, replica_id: &str, lsn: u64) {
        let mut state = self.state.lock().expect("commit waiter mutex");
        let entry = state.durable_lsn.entry(replica_id.to_string()).or_insert(0);
        if lsn > *entry {
            *entry = lsn;
            self.cond.notify_all();
        }
    }

    /// Best-effort cleanup when a replica disconnects. Removes its
    /// durable LSN from the map so it doesn't artificially inflate
    /// `ack_n` counts. Wakes waiters because the count of replicas
    /// at the target may have decreased — they need to re-evaluate
    /// against the new reality (some will start failing if their
    /// margin was thin).
    pub fn drop_replica(&self, replica_id: &str) {
        let mut state = self.state.lock().expect("commit waiter mutex");
        if state.durable_lsn.remove(replica_id).is_some() {
            self.cond.notify_all();
        }
    }

    /// Snapshot of the current durable-LSN map. Useful for
    /// observability and tests; doesn't unblock waiters.
    pub fn snapshot(&self) -> Vec<(String, u64)> {
        let state = self.state.lock().expect("commit waiter mutex");
        let mut v: Vec<(String, u64)> = state
            .durable_lsn
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Block until at least `required` replicas have durable LSN
    /// `>= target_lsn`, or `timeout` expires. `required == 0` is a
    /// no-op (returns `NotRequired` instantly).
    ///
    /// Uses `Condvar::wait_timeout` to avoid spinning. On every wake
    /// (whether from an ack or a spurious wakeup), we recompute the
    /// count and either return or wait again with the remaining
    /// budget.
    pub fn await_acks(&self, target_lsn: u64, required: u32, timeout: Duration) -> AwaitOutcome {
        if required == 0 {
            self.metrics
                .not_required_total
                .fetch_add(1, Ordering::Relaxed);
            return AwaitOutcome::NotRequired;
        }
        let started = Instant::now();
        let deadline = started + timeout;
        let mut state = self.state.lock().expect("commit waiter mutex");
        loop {
            let observed = count_at_or_past(&state.durable_lsn, target_lsn);
            if observed >= required {
                self.record_outcome_metrics(true, started);
                return AwaitOutcome::Reached(observed);
            }
            let now = Instant::now();
            if now >= deadline {
                self.record_outcome_metrics(false, started);
                return AwaitOutcome::TimedOut { observed, required };
            }
            let remaining = deadline - now;
            let (next_state, _wait_result) = self
                .cond
                .wait_timeout(state, remaining)
                .expect("commit waiter condvar");
            state = next_state;
        }
    }

    fn record_outcome_metrics(&self, reached: bool, started: Instant) {
        let elapsed = started.elapsed().as_micros() as u64;
        self.metrics
            .last_wait_micros
            .store(elapsed, Ordering::Relaxed);
        if reached {
            self.metrics.reached_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.timed_out_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot of outcome counters for /metrics + tests.
    pub fn metrics_snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.metrics.reached_total.load(Ordering::Relaxed),
            self.metrics.timed_out_total.load(Ordering::Relaxed),
            self.metrics.not_required_total.load(Ordering::Relaxed),
            self.metrics.last_wait_micros.load(Ordering::Relaxed),
        )
    }
}

fn count_at_or_past(map: &HashMap<String, u64>, target_lsn: u64) -> u32 {
    map.values().filter(|lsn| **lsn >= target_lsn).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn required_zero_is_immediate_no_op() {
        let w = CommitWaiter::new();
        let r = w.await_acks(100, 0, Duration::from_secs(60));
        assert_eq!(r, AwaitOutcome::NotRequired);
    }

    #[test]
    fn reaches_threshold_with_existing_acks() {
        let w = CommitWaiter::new();
        w.record_replica_ack("a", 200);
        w.record_replica_ack("b", 200);
        let r = w.await_acks(150, 2, Duration::from_millis(10));
        assert_eq!(r, AwaitOutcome::Reached(2));
    }

    #[test]
    fn times_out_when_no_one_has_acked() {
        let w = CommitWaiter::new();
        w.record_replica_ack("a", 100);
        let r = w.await_acks(500, 1, Duration::from_millis(20));
        match r {
            AwaitOutcome::TimedOut { observed, required } => {
                assert_eq!(observed, 0);
                assert_eq!(required, 1);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[test]
    fn ack_arriving_during_wait_unblocks_caller() {
        let w = Arc::new(CommitWaiter::new());
        let waiter = Arc::clone(&w);
        let handle = thread::spawn(move || waiter.await_acks(1000, 1, Duration::from_secs(2)));
        // Give the waiter a moment to enter the condvar wait.
        thread::sleep(Duration::from_millis(50));
        w.record_replica_ack("late", 1000);
        let outcome = handle.join().expect("waiter thread");
        assert_eq!(outcome, AwaitOutcome::Reached(1));
    }

    #[test]
    fn ack_idempotent_does_not_double_count() {
        let w = CommitWaiter::new();
        w.record_replica_ack("a", 50);
        w.record_replica_ack("a", 50);
        w.record_replica_ack("a", 50);
        let r = w.await_acks(50, 1, Duration::from_millis(5));
        assert_eq!(r, AwaitOutcome::Reached(1));
        // Threshold of 2 still fails — only one replica is registered.
        let r2 = w.await_acks(50, 2, Duration::from_millis(20));
        assert!(matches!(
            r2,
            AwaitOutcome::TimedOut {
                observed: 1,
                required: 2
            }
        ));
    }

    #[test]
    fn ack_only_advances_lsn_forward() {
        let w = CommitWaiter::new();
        w.record_replica_ack("a", 200);
        // Older ack must not regress the recorded LSN.
        w.record_replica_ack("a", 100);
        let snap = w.snapshot();
        assert_eq!(snap, vec![("a".to_string(), 200)]);
    }

    #[test]
    fn drop_replica_removes_from_count() {
        let w = CommitWaiter::new();
        w.record_replica_ack("a", 100);
        w.record_replica_ack("b", 100);
        w.drop_replica("a");
        let r = w.await_acks(100, 2, Duration::from_millis(20));
        assert!(matches!(
            r,
            AwaitOutcome::TimedOut {
                observed: 1,
                required: 2
            }
        ));
    }

    #[test]
    fn metrics_count_each_outcome_kind() {
        let w = CommitWaiter::new();
        // not_required
        w.await_acks(100, 0, Duration::from_millis(5));
        // timed_out
        w.await_acks(100, 1, Duration::from_millis(5));
        // reached
        w.record_replica_ack("a", 100);
        w.await_acks(100, 1, Duration::from_millis(5));

        let (reached, timed_out, not_required, last_micros) = w.metrics_snapshot();
        assert_eq!(reached, 1, "one Reached call");
        assert_eq!(timed_out, 1, "one TimedOut call");
        assert_eq!(not_required, 1, "one NotRequired call");
        // last_wait_micros is set on Reached/TimedOut, NotRequired
        // skips the gauge so the most recent measurement reflects
        // an actual wait.
        assert!(last_micros > 0, "last_wait_micros must be set");
    }
}
