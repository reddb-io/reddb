//! Issue #1609 — queue-shaped `CLAIM` routes through `QueueLifecycle`.
//!
//! A `UPDATE q CLAIM LIMIT n ...` against a *queue* collection is a
//! delivery acquisition under the `QueueLifecycle` state machine (ADR
//! 0020), not a raw UPDATE of the underlying queue rows. These tests
//! exercise the runtime seam so every transport inherits the behaviour:
//!
//! - a queue `CLAIM` acquires up to N pending deliveries (returning the
//!   opaque `delivery_id`) and locks them so a concurrent claim skips
//!   them — proving the lifecycle owns delivery state, not a raw mutation;
//! - the acquired `delivery_id` is a real lifecycle handle: `QUEUE ACK`
//!   retires it, preserving ACK/NACK/retry/DLQ semantics;
//! - a `CLAIM` shape the delivery seam cannot express (descending
//!   `ORDER BY`, or `CLAIM EXACT`) returns a clear `InvalidOperation`
//!   rather than silently mutating queue storage.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn delivery_ids(result: &reddb_server::RuntimeQueryResult) -> Vec<String> {
    result
        .result
        .records
        .iter()
        .map(|rec| match rec.get("delivery_id") {
            Some(Value::Text(s)) => s.to_string(),
            other => panic!("expected text delivery_id, got {other:?}"),
        })
        .collect()
}

#[test]
fn queue_claim_acquires_deliveries_and_locks_them() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qclaim");
    exec(&rt, "QUEUE PUSH qclaim 'job-1'");
    exec(&rt, "QUEUE PUSH qclaim 'job-2'");
    exec(&rt, "QUEUE PUSH qclaim 'job-3'");

    // Queue-shaped CLAIM: the SET / WHERE are subsumed by delivery
    // acquisition — the lifecycle transitions the messages to pending.
    let first = rt
        .execute_query(
            "UPDATE qclaim SET status = 'processing' WHERE status = 'pending' \
             CLAIM LIMIT 2 ORDER BY enqueued_at ASC",
        )
        .expect("queue claim succeeds");
    assert_eq!(first.affected_rows, 2, "claims up to the requested limit");
    let first_ids = delivery_ids(&first);
    assert_eq!(
        first_ids.len(),
        2,
        "each claimed message carries a delivery_id"
    );

    // The first two deliveries are locked under the lifecycle, so a second
    // claim skips them and only acquires the one remaining message. A raw
    // storage mutation would not hold delivery locks this way.
    let second = rt
        .execute_query(
            "UPDATE qclaim SET status = 'processing' CLAIM LIMIT 5 ORDER BY enqueued_at ASC",
        )
        .expect("second queue claim succeeds");
    assert_eq!(
        second.affected_rows, 1,
        "the two pending deliveries stay locked; only one message is left"
    );
}

#[test]
fn queue_claim_delivery_id_is_a_real_lifecycle_handle() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qack");
    exec(&rt, "QUEUE PUSH qack 'payload-1'");

    let claimed = rt
        .execute_query(
            "UPDATE qack SET status = 'processing' CLAIM LIMIT 1 ORDER BY enqueued_at ASC",
        )
        .expect("queue claim succeeds");
    assert_eq!(claimed.affected_rows, 1);
    let delivery_id = delivery_ids(&claimed)
        .into_iter()
        .next()
        .expect("a delivery was acquired");

    // ACK against the acquired delivery_id retires the message through the
    // QueueLifecycle — proving the CLAIM produced a genuine delivery, and
    // that ACK/NACK/retry/DLQ semantics stay under lifecycle authority.
    exec(
        &rt,
        &format!("QUEUE ACK qack WITH delivery_id = '{delivery_id}'"),
    );

    // The queue is now drained: no message remains to claim.
    let empty = rt
        .execute_query(
            "UPDATE qack SET status = 'processing' CLAIM LIMIT 5 ORDER BY enqueued_at ASC",
        )
        .expect("claim on drained queue succeeds");
    assert_eq!(empty.affected_rows, 0, "acked delivery is not re-claimable");
}

#[test]
fn queue_claim_descending_order_is_rejected() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdesc");
    exec(&rt, "QUEUE PUSH qdesc 'job-1'");

    let err = rt
        .execute_query(
            "UPDATE qdesc SET status = 'processing' \
             CLAIM LIMIT 1 ORDER BY enqueued_at DESC",
        )
        .expect_err("descending ORDER BY conflicts with FIFO delivery");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("cannot be expressed") && msg.contains("FIFO"),
        "expected a clear InvalidOperation about FIFO delivery order, got: {msg}"
    );
}

#[test]
fn queue_claim_exact_is_rejected() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qexact");
    exec(&rt, "QUEUE PUSH qexact 'job-1'");

    let err = rt
        .execute_query(
            "UPDATE qexact SET status = 'processing' CLAIM EXACT 3 ORDER BY enqueued_at ASC",
        )
        .expect_err("CLAIM EXACT cannot be expressed through queue delivery");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("CLAIM EXACT") && msg.contains("cannot be expressed"),
        "expected a clear InvalidOperation about CLAIM EXACT, got: {msg}"
    );
}
