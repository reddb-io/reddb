//! Issue #727 / slice B of PRD #718 — runtime acceptance for the
//! server cap and the autocommit-only contract on
//! `QUEUE READ … WAIT <duration>`.
//!
//! Two guarantees the brief calls out:
//!
//!   1. `WAIT` durations above the server cap
//!      (`red.config.queue.max_wait_ms`) are rejected before any
//!      waiter is registered, with an error message that names the
//!      cap so operators can see why their statement was refused.
//!   2. `WAIT` issued inside an explicit `BEGIN`/`COMMIT` transaction
//!      is rejected — the wait is autocommit-only because a parked
//!      reader inside a transaction would hold its txn-local state
//!      hostage to a producer that cannot make progress until the
//!      reader either commits or rolls back.

use reddb_server::{RedDBOptions, RedDBRuntime};
use std::time::{Duration, Instant};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn wait_above_default_cap_is_rejected_with_explicit_message() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qwait_cap_default");
    exec(&rt, "QUEUE GROUP CREATE qwait_cap_default workers");

    // Default cap is 60_000ms; 999h is far above that.
    let started = Instant::now();
    let err = rt
        .execute_query("QUEUE READ qwait_cap_default GROUP workers CONSUMER c1 COUNT 1 WAIT 999h")
        .expect_err("WAIT above cap should reject");
    let elapsed = started.elapsed();

    let msg = format!("{err}");
    assert!(
        msg.contains("red.config.queue.max_wait_ms"),
        "error should name the cap key, got: {msg:?}"
    );
    assert!(
        msg.contains("60000"),
        "error should name the active cap value, got: {msg:?}"
    );
    // No waiter should be registered — the reject path must not park.
    assert!(
        elapsed < Duration::from_millis(500),
        "rejection should be immediate, elapsed={elapsed:?}"
    );
}

#[test]
fn wait_above_operator_set_cap_is_rejected_with_active_cap_value() {
    let rt = runtime();
    // Tighten the cap to 250ms; anything above it must reject and
    // the error must name the *active* (overridden) cap, not the
    // default.
    exec(&rt, "SET CONFIG red.config.queue.max_wait_ms = 250");
    exec(&rt, "CREATE QUEUE qwait_cap_tight");
    exec(&rt, "QUEUE GROUP CREATE qwait_cap_tight workers");

    let err = rt
        .execute_query("QUEUE READ qwait_cap_tight GROUP workers CONSUMER c1 COUNT 1 WAIT 1s")
        .expect_err("WAIT above tightened cap should reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("250"),
        "error should reflect the operator-set cap (250), got: {msg:?}"
    );
    assert!(
        msg.contains("red.config.queue.max_wait_ms"),
        "error should name the cap key, got: {msg:?}"
    );

    // And right at the cap (≤ cap) is still accepted — proves the
    // rejection is strict-greater-than, not off-by-one.
    let read = rt
        .execute_query("QUEUE READ qwait_cap_tight GROUP workers CONSUMER c1 COUNT 1 WAIT 250ms")
        .expect("WAIT at the cap should be accepted");
    assert!(read.result.records.is_empty());
}

#[test]
fn wait_inside_explicit_transaction_is_rejected_as_autocommit_only() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qwait_txn");
    exec(&rt, "QUEUE GROUP CREATE qwait_txn workers");

    exec(&rt, "BEGIN");
    let started = Instant::now();
    let err = rt
        .execute_query("QUEUE READ qwait_txn GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect_err("WAIT inside BEGIN/COMMIT should reject");
    let elapsed = started.elapsed();
    let msg = format!("{err}");

    assert!(
        msg.to_lowercase().contains("autocommit")
            || msg.to_lowercase().contains("explicit transaction"),
        "error should explain WAIT is autocommit-only, got: {msg:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "rejection should be immediate, no parking, elapsed={elapsed:?}"
    );

    // The connection's transaction is still open — rollback cleanly
    // so the test leaves no in-flight txn behind.
    exec(&rt, "ROLLBACK");

    // After rollback we are back in autocommit; the same WAIT now
    // succeeds (returns empty after the budget). Confirms the
    // rejection is conditioned on txn state, not the statement.
    let read = rt
        .execute_query("QUEUE READ qwait_txn GROUP workers CONSUMER c1 COUNT 1 WAIT 100ms")
        .expect("autocommit WAIT after rollback should be accepted");
    assert!(read.result.records.is_empty());
}

#[test]
fn wait_with_no_clause_is_unaffected_by_cap_and_txn_checks() {
    // The cap + txn checks are gated on `wait_ms.is_some()`. A bare
    // `QUEUE READ` (no WAIT) inside a transaction is the existing,
    // pre-#727 behaviour — it must remain accepted.
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qwait_none");
    exec(&rt, "QUEUE GROUP CREATE qwait_none workers");

    exec(&rt, "BEGIN");
    let read = rt
        .execute_query("QUEUE READ qwait_none GROUP workers CONSUMER c1 COUNT 1")
        .expect("non-WAIT read inside a txn is still accepted");
    assert!(read.result.records.is_empty());
    exec(&rt, "COMMIT");
}
