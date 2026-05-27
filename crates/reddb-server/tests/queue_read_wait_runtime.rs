//! Issue #728 / slice C of PRD #718 — runtime acceptance for
//! `QUEUE READ … WAIT <duration>`.
//!
//! These tests pin the five guarantees the brief calls out:
//!
//!   1. Immediate-available read returns without parking.
//!   2. Empty queue + `WAIT <ms>` returns empty after ~the budget.
//!   3. Enqueue + COMMIT during the wait wakes the parked reader;
//!      ROLLBACK does not (rolled-back enqueues never deliver).
//!   4. Five concurrent waiters on the same queue + a single enqueue:
//!      normal delivery arbitration assigns the message to exactly
//!      one reader (the others re-park or time out).
//!   5. `QueueWaitRegistry::cancel_all()` during a wait surfaces an
//!      explicit cancellation error to the parked caller.
//!
//! The runtime path under test is `RedDBRuntime::execute_query` →
//! `QueueCommand::GroupRead { wait_ms: Some(_) }` →
//! `group_read_with_optional_wait`. Every transport (HTTP, gRPC,
//! redwire, postgres-wire) dispatches through this same parser entry
//! point — pinning behaviour here pins it for all four.

use reddb_server::{RedDBOptions, RedDBRuntime};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn wait_returns_immediately_when_message_already_present() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qwait_imm");
    exec(&rt, "QUEUE GROUP CREATE qwait_imm workers");
    exec(&rt, "QUEUE PUSH qwait_imm 'ready'");

    let started = Instant::now();
    let read = rt
        .execute_query("QUEUE READ qwait_imm GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect("read");
    let elapsed = started.elapsed();

    assert_eq!(
        read.result.records.len(),
        1,
        "immediate-available read should deliver one message"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "immediate read should not park (elapsed={elapsed:?})"
    );
}

#[test]
fn wait_returns_empty_after_budget_when_queue_stays_empty() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qwait_empty");
    exec(&rt, "QUEUE GROUP CREATE qwait_empty workers");

    let started = Instant::now();
    let read = rt
        .execute_query("QUEUE READ qwait_empty GROUP workers CONSUMER c1 COUNT 1 WAIT 200ms")
        .expect("read");
    let elapsed = started.elapsed();

    assert!(
        read.result.records.is_empty(),
        "timeout should return empty projection, got {:?}",
        read.result.records
    );
    assert!(
        elapsed >= Duration::from_millis(180),
        "should park at least ~the WAIT budget, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "should not stall past the budget, elapsed={elapsed:?}"
    );
}

#[test]
fn commit_during_wait_wakes_reader_but_rollback_does_not() {
    let rt = Arc::new(runtime());
    exec(&rt, "CREATE QUEUE qwait_commit");
    exec(&rt, "QUEUE GROUP CREATE qwait_commit workers");

    // ------------------------------------------------------------------
    // First: rollback must NOT wake — we exercise that the read still
    // sees the queue as empty even though a producer pushed and rolled
    // back inside the WAIT window. The reader's WAIT budget elapses
    // and we get an empty projection.
    // ------------------------------------------------------------------
    let producer_rt = rt.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        producer_rt.execute_query("BEGIN").expect("begin");
        producer_rt
            .execute_query("QUEUE PUSH qwait_commit 'doomed'")
            .expect("push");
        producer_rt.execute_query("ROLLBACK").expect("rollback");
    });
    let started = Instant::now();
    let read = rt
        .execute_query("QUEUE READ qwait_commit GROUP workers CONSUMER c1 COUNT 1 WAIT 400ms")
        .expect("read");
    let elapsed = started.elapsed();
    producer.join().unwrap();
    assert!(
        read.result.records.is_empty(),
        "rolled-back enqueue must not deliver: {:?}",
        read.result.records
    );
    assert!(
        elapsed >= Duration::from_millis(380),
        "rollback should not short-circuit the WAIT (elapsed={elapsed:?})"
    );

    // ------------------------------------------------------------------
    // Now COMMIT: the post-commit notify wakes the parked reader and
    // we get the message back well before the WAIT budget elapses.
    // ------------------------------------------------------------------
    let producer_rt = rt.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        producer_rt.execute_query("BEGIN").expect("begin");
        producer_rt
            .execute_query("QUEUE PUSH qwait_commit 'live'")
            .expect("push");
        producer_rt.execute_query("COMMIT").expect("commit");
    });
    let started = Instant::now();
    let read = rt
        .execute_query("QUEUE READ qwait_commit GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect("read");
    let elapsed = started.elapsed();
    producer.join().unwrap();
    assert_eq!(
        read.result.records.len(),
        1,
        "committed enqueue must wake the waiter and deliver, got {:?}",
        read.result.records
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "commit notify should wake well before the WAIT budget (elapsed={elapsed:?})"
    );
}

#[test]
fn concurrent_waiters_single_enqueue_lets_arbitration_pick_one_winner() {
    let rt = Arc::new(runtime());
    exec(&rt, "CREATE QUEUE qwait_arb");
    exec(&rt, "QUEUE GROUP CREATE qwait_arb workers");

    let mut handles = Vec::new();
    for i in 0..5 {
        let rt = rt.clone();
        handles.push(thread::spawn(move || {
            let sql = format!("QUEUE READ qwait_arb GROUP workers CONSUMER c{i} COUNT 1 WAIT 2s");
            rt.execute_query(&sql).expect("read")
        }));
    }
    thread::sleep(Duration::from_millis(150));
    let pusher = rt.clone();
    thread::spawn(move || exec(&pusher, "QUEUE PUSH qwait_arb 'one'"))
        .join()
        .unwrap();

    let mut got = 0usize;
    for h in handles {
        let res = h.join().expect("waiter joined");
        got += res.result.records.len();
    }
    assert_eq!(
        got, 1,
        "single enqueue must yield exactly one delivery across wake-all waiters, got {got}"
    );
}

#[test]
fn shutdown_during_wait_returns_explicit_cancellation_error() {
    let rt = Arc::new(runtime());
    exec(&rt, "CREATE QUEUE qwait_cancel");
    exec(&rt, "QUEUE GROUP CREATE qwait_cancel workers");

    let canceler_rt = rt.clone();
    let registry = canceler_rt.queue_wait_registry();
    let canceler = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        registry.cancel_all();
    });

    let err = rt
        .execute_query("QUEUE READ qwait_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect_err("cancellation should surface as Err");
    canceler.join().unwrap();

    let msg = format!("{err}");
    assert!(
        msg.contains("WAIT cancelled") || msg.contains("shutting down"),
        "cancellation error should be explicit, got {msg:?}"
    );

    // Reset for any test running afterwards so a shared process-wide
    // counter (none today, but cheap insurance) doesn't smear state.
    rt.queue_wait_registry().reset_cancelled();
}
