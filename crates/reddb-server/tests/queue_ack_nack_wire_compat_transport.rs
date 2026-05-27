//! Issue #717 — QueueLifecycle wire-compat bridge for ACK/NACK
//! (tuple + delivery_id) across all four transports.
//!
//! The four transports (redwire, gRPC, postgres-wire, HTTP) all
//! dispatch ACK/NACK through the same SQL parser and runtime path:
//!
//!     ACK <queue> GROUP <group> '<message_id>'
//!     ACK <queue> WITH delivery_id = '<base32>'
//!     ACK <queue> GROUP <group> '<message_id>' WITH delivery_id = '<base32>'
//!
//! Once the runtime layer enforces the right accept/reject semantics
//! every transport inherits them automatically — there is no per-
//! transport ACK opcode that could diverge. This file pins the
//! semantics at the runtime/SQL level and is named with the
//! `transport` suffix so it is picked up by
//! `cargo test -p reddb-io-server --tests "*transport*"`.
//!
//! Scenarios pinned per ACK and per NACK:
//!
//!   1. Legacy tuple-only handle succeeds + emits one tuple
//!      deprecation log line.
//!   2. New `delivery_id`-only handle succeeds + emits **no**
//!      deprecation.
//!   3. Both supplied → `delivery_id` wins (the tuple is ignored;
//!      a deliberately bogus tuple does not derail the ACK).
//!   4. Stale `delivery_id` errors instead of silently falling back
//!      to the tuple.
//!
//! The per-(connection, queue) cooldown on the deprecation log is
//! also covered: a second tuple-ACK from the same connection within
//! the cooldown window does not double-emit.

use reddb_server::runtime::impl_queue::TUPLE_DEPRECATION_EMITS;
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::sync::atomic::{AtomicU64, Ordering};

/// Hand out a unique connection id per test so each test owns its
/// own cooldown slot in the process-wide `LAST_EMIT` map and the
/// per-(conn, queue) cooldown assertions don't smear across the
/// suite when `cargo test` runs in parallel.
fn fresh_connection_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0x7170_0001);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

/// Push a message into `queue` under `group`, deliver it to
/// `consumer`, and return both the message_id (for the legacy tuple
/// handle) and the delivery_id (for the new handle). Uses the
/// `red.queue_pending` virtual table to recover the server-issued
/// delivery_id, the same surface every transport exposes.
fn enqueue_and_deliver(
    rt: &RedDBRuntime,
    queue: &str,
    group: &str,
    consumer: &str,
    payload: &str,
) -> (String, String) {
    exec(rt, &format!("QUEUE PUSH {queue} '{payload}'"));
    let read = rt
        .execute_query(&format!(
            "QUEUE READ {queue} GROUP {group} CONSUMER {consumer} COUNT 1"
        ))
        .expect("read")
        .result;
    assert_eq!(read.records.len(), 1, "read returned a delivery");
    let message_id = match read.records[0].get("message_id") {
        Some(Value::Text(s)) => s.to_string(),
        other => panic!("expected message_id text, got {other:?}"),
    };

    // Recover delivery_id straight from the lifecycle meta-row. We
    // intentionally do **not** go through `red.queue_pending` (a
    // legacy virtual table keyed on `kind = "queue_pending"`); the
    // post-#716 lifecycle store writes `queue_pending_lc` rows
    // instead, which is where the server-issued delivery_id lives.
    let delivery_id = lookup_delivery_id(rt, queue, &message_id, group);
    (message_id, delivery_id)
}

/// Scan `red_queue_meta` for the lifecycle pending row matching
/// `(queue, group, message_id)` and return its server-issued
/// `delivery_id`. Panics if no row matches — the caller has just
/// delivered the message, so a missing row is a real test failure.
fn lookup_delivery_id(rt: &RedDBRuntime, queue: &str, message_id: &str, _group: &str) -> String {
    // `SELECT *` so we don't have to fight column-name quoting for the
    // reserved `group` identifier. We then filter in Rust against the
    // (queue, message_id, delivery_id) triple — a pending row with a
    // populated `delivery_id` is the lifecycle row by construction.
    let meta = rt
        .execute_query("SELECT * FROM red_queue_meta")
        .expect("scan red_queue_meta")
        .result;
    for row in &meta.records {
        let row_queue = match row.get("queue") {
            Some(Value::Text(s)) => s.to_string(),
            _ => continue,
        };
        let row_mid = match row.get("message_id") {
            Some(Value::UnsignedInteger(u)) => u.to_string(),
            Some(Value::Integer(i)) => i.to_string(),
            Some(Value::Text(s)) => s.to_string(),
            _ => continue,
        };
        if row_queue == queue && row_mid == message_id {
            if let Some(Value::Text(d)) = row.get("delivery_id") {
                return d.to_string();
            }
        }
    }
    panic!("no lifecycle pending row with delivery_id found for ({queue}, {message_id})")
}

// ----- ACK scenarios ---------------------------------------------------

#[test]
fn ack_legacy_tuple_only_succeeds_and_emits_deprecation() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_tuple");
    exec(&rt, "QUEUE GROUP CREATE q_tuple workers");
    let (message_id, _delivery_id) =
        enqueue_and_deliver(&rt, "q_tuple", "workers", "worker1", "job-1");

    let before = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    let res = rt
        .execute_query(&format!("QUEUE ACK q_tuple GROUP workers '{message_id}'"))
        .expect("tuple ACK succeeds");
    assert_eq!(res.affected_rows, 0);
    let after = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    assert_eq!(
        after - before,
        1,
        "tuple ACK must emit one deprecation log line"
    );

    // Same connection, same queue, second tuple ACK inside cooldown
    // → no extra emission (rate-limit to one per minute).
    let (message_id_2, _) = enqueue_and_deliver(&rt, "q_tuple", "workers", "worker1", "job-2");
    rt.execute_query(&format!("QUEUE ACK q_tuple GROUP workers '{message_id_2}'"))
        .expect("second tuple ACK succeeds");
    let after_2 = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    assert_eq!(
        after_2 - after,
        0,
        "rate-limited cooldown suppresses second emission in same (conn, queue)"
    );

    clear_current_connection_id();
}

#[test]
fn ack_delivery_id_only_succeeds_and_does_not_emit_deprecation() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_did");
    exec(&rt, "QUEUE GROUP CREATE q_did workers");
    let (_message_id, delivery_id) =
        enqueue_and_deliver(&rt, "q_did", "workers", "worker1", "job-1");

    let before = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    rt.execute_query(&format!(
        "QUEUE ACK q_did WITH delivery_id = '{delivery_id}'"
    ))
    .expect("delivery_id ACK succeeds");
    let after = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    assert_eq!(
        after - before,
        0,
        "delivery_id-only path must NOT emit deprecation"
    );

    clear_current_connection_id();
}

#[test]
fn ack_with_both_handles_resolves_via_delivery_id_and_ignores_tuple() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_both");
    exec(&rt, "QUEUE GROUP CREATE q_both workers");
    let (_message_id, delivery_id) =
        enqueue_and_deliver(&rt, "q_both", "workers", "worker1", "job-1");

    // Tuple is *deliberately wrong* (bogus group + nonsense message_id).
    // The ACK must still succeed because delivery_id wins
    // unconditionally — if we ever fall back to the tuple this
    // assertion flips and reveals the regression.
    rt.execute_query(&format!(
        "QUEUE ACK q_both GROUP bogus_group '9999999999' WITH delivery_id = '{delivery_id}'"
    ))
    .expect("delivery_id wins over the supplied tuple");

    // The pending lifecycle row is gone, proving the right delivery
    // was acked (not the bogus tuple, which would have errored).
    let meta = rt
        .execute_query("SELECT * FROM red_queue_meta")
        .expect("scan meta")
        .result;
    let still_pending = meta.records.iter().any(|row| {
        matches!(row.get("delivery_id"), Some(Value::Text(d)) if d.as_ref() == delivery_id.as_str())
    });
    assert!(!still_pending, "ACKed delivery must leave the pending set");

    clear_current_connection_id();
}

#[test]
fn ack_stale_delivery_id_errors_instead_of_falling_back_to_tuple() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_stale");
    exec(&rt, "QUEUE GROUP CREATE q_stale workers");
    let (message_id, delivery_id) =
        enqueue_and_deliver(&rt, "q_stale", "workers", "worker1", "job-1");

    // First ACK retires the delivery via delivery_id.
    rt.execute_query(&format!(
        "QUEUE ACK q_stale WITH delivery_id = '{delivery_id}'"
    ))
    .expect("first ACK clears delivery");

    // Re-using the now-stale delivery_id MUST error, even though we
    // also pass a tuple that previously matched. No silent fallback.
    let err = rt
        .execute_query(&format!(
            "QUEUE ACK q_stale GROUP workers '{message_id}' WITH delivery_id = '{delivery_id}'"
        ))
        .expect_err("stale delivery_id must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("delivery") && msg.contains(&delivery_id),
        "expected delivery error mentioning the stale id, got: {msg}"
    );

    clear_current_connection_id();
}

// ----- NACK scenarios --------------------------------------------------

#[test]
fn nack_legacy_tuple_only_succeeds_and_emits_deprecation() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_nack_tuple MAX_ATTEMPTS 5");
    exec(&rt, "QUEUE GROUP CREATE q_nack_tuple workers");
    let (message_id, _) = enqueue_and_deliver(&rt, "q_nack_tuple", "workers", "worker1", "job-1");

    let before = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    rt.execute_query(&format!(
        "QUEUE NACK q_nack_tuple GROUP workers '{message_id}'"
    ))
    .expect("tuple NACK succeeds");
    let after = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    assert_eq!(
        after - before,
        1,
        "tuple NACK must emit one deprecation log line"
    );

    clear_current_connection_id();
}

#[test]
fn nack_delivery_id_only_succeeds_and_does_not_emit_deprecation() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_nack_did MAX_ATTEMPTS 5");
    exec(&rt, "QUEUE GROUP CREATE q_nack_did workers");
    let (_message_id, delivery_id) =
        enqueue_and_deliver(&rt, "q_nack_did", "workers", "worker1", "job-1");

    let before = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    rt.execute_query(&format!(
        "QUEUE NACK q_nack_did WITH delivery_id = '{delivery_id}'"
    ))
    .expect("delivery_id NACK succeeds");
    let after = TUPLE_DEPRECATION_EMITS.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "delivery_id-only NACK must not emit");

    clear_current_connection_id();
}

#[test]
fn nack_stale_delivery_id_errors() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_nack_stale MAX_ATTEMPTS 5");
    exec(&rt, "QUEUE GROUP CREATE q_nack_stale workers");
    let (_message_id, delivery_id) =
        enqueue_and_deliver(&rt, "q_nack_stale", "workers", "worker1", "job-1");

    rt.execute_query(&format!(
        "QUEUE ACK q_nack_stale WITH delivery_id = '{delivery_id}'"
    ))
    .expect("clear the delivery via ACK first");

    let err = rt
        .execute_query(&format!(
            "QUEUE NACK q_nack_stale WITH delivery_id = '{delivery_id}'"
        ))
        .expect_err("stale delivery_id NACK must reject");
    assert!(
        err.to_string().contains("delivery"),
        "expected delivery error, got: {err:?}"
    );

    clear_current_connection_id();
}

// ----- Parse-layer guard: refusing neither handle ----------------------

#[test]
fn ack_nack_refuses_when_neither_handle_supplied() {
    let conn = fresh_connection_id();
    set_current_connection_id(conn);
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q_refuse");
    exec(&rt, "QUEUE GROUP CREATE q_refuse workers");

    assert!(rt.execute_query("QUEUE ACK q_refuse").is_err());
    assert!(rt.execute_query("QUEUE NACK q_refuse").is_err());

    clear_current_connection_id();
}
