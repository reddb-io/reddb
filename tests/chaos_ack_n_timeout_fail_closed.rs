//! Chaos test: ack_n timeout when no replica acks (PLAN.md Phase 11.4 + 8 slice).
//!
//! Verifies the CommitWaiter contract end-to-end with no actual
//! gRPC stack:
//!   * `await_acks` with `required > 0` and no acks recorded must
//!     return `TimedOut` within the configured budget.
//!   * The `last_wait_micros` gauge is populated.
//!   * The `timed_out_total` counter increments.
//!   * A late ack arriving after the deadline does NOT retroactively
//!     unblock a previous waiter (already returned).
//!   * Adding the required acks before the wait returns `Reached`.
//!
//! This is the primitive an HTTP / gRPC handler relies on when the
//! operator sets `RED_PRIMARY_COMMIT_POLICY=ack_n=N` +
//! `RED_COMMIT_FAIL_ON_TIMEOUT=true`. Without this guarantee the
//! 504 / `Status::deadline_exceeded` mapping at the surface is
//! meaningless.

use reddb::replication::commit_waiter::{AwaitOutcome, CommitWaiter};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn ack_n_times_out_when_no_replica_acks() {
    let w = CommitWaiter::new();
    let outcome = w.await_acks(100, 1, Duration::from_millis(20));
    match outcome {
        AwaitOutcome::TimedOut { observed, required } => {
            assert_eq!(observed, 0, "no acks recorded");
            assert_eq!(required, 1);
        }
        other => panic!("expected TimedOut, got {other:?}"),
    }
    let (reached, timed_out, _, last_micros) = w.metrics_snapshot();
    assert_eq!(reached, 0);
    assert_eq!(timed_out, 1);
    assert!(
        last_micros > 0,
        "TimedOut must populate last_wait_micros so /metrics reflects it"
    );
}

#[test]
fn late_ack_does_not_retroactively_unblock_prior_waiter() {
    let w = Arc::new(CommitWaiter::new());

    // First waiter — deadline 20ms, no ack arrives in window.
    let w1 = Arc::clone(&w);
    let r1 = std::thread::spawn(move || w1.await_acks(50, 1, Duration::from_millis(20)));

    // Sleep past the first waiter's deadline before recording the ack.
    std::thread::sleep(Duration::from_millis(40));
    w.record_replica_ack("late-replica", 50);

    // First waiter must have already returned TimedOut — late ack
    // does not retroactively change that.
    let outcome1 = r1.join().expect("waiter thread");
    assert!(
        matches!(outcome1, AwaitOutcome::TimedOut { observed: 0, required: 1 }),
        "first waiter must have timed out before the late ack arrived; got {outcome1:?}"
    );

    // A new waiter sees the recorded ack immediately.
    let outcome2 = w.await_acks(50, 1, Duration::from_millis(20));
    assert_eq!(outcome2, AwaitOutcome::Reached(1));

    let (reached, timed_out, _, _) = w.metrics_snapshot();
    assert_eq!(reached, 1, "second wait reached");
    assert_eq!(timed_out, 1, "first wait timed out");
}

#[test]
fn ack_n_requires_distinct_replicas_not_duplicate_acks() {
    // Two acks from the SAME replica should not satisfy required=2.
    // The bucket is keyed by replica_id; same id = same slot.
    let w = CommitWaiter::new();
    w.record_replica_ack("solo", 100);
    w.record_replica_ack("solo", 100);
    w.record_replica_ack("solo", 100);
    let outcome = w.await_acks(100, 2, Duration::from_millis(20));
    match outcome {
        AwaitOutcome::TimedOut { observed, required } => {
            assert_eq!(observed, 1, "one replica acking 3 times still counts as 1");
            assert_eq!(required, 2);
        }
        other => panic!("expected TimedOut, got {other:?}"),
    }
}
