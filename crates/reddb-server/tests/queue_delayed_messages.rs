//! Issue #722 — runtime acceptance for per-message delayed availability.
//!
//! Producers can attach a per-message `DELAY <duration>` (relative) or
//! `AVAILABLE AT <unix_ms>` (absolute) clause to `QUEUE PUSH`. Delayed
//! messages remain durable and inspectable but are skipped by
//! `QUEUE READ`, `QUEUE POP`, and the legacy `QUEUE PEEK` consumer path
//! until they become due. `QUEUE READ … WAIT` honours the delay and
//! delivers the message as soon as it becomes due (capped by the
//! caller's WAIT budget).
//!
//! These tests cover the runtime path under `execute_query` so all four
//! transports inherit the behaviour by construction.

use reddb_server::storage::schema::Value;
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
fn delayed_message_is_not_returned_by_read_before_due() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_read");
    exec(&rt, "QUEUE GROUP CREATE qdelay_read workers");
    exec(&rt, "QUEUE PUSH qdelay_read 'late' DELAY 5s");

    let read = rt
        .execute_query("QUEUE READ qdelay_read GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert!(
        read.result.records.is_empty(),
        "delayed message must not be deliverable before its due time, got {:?}",
        read.result.records
    );
}

#[test]
fn delayed_message_is_not_returned_by_pop_before_due() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_pop");
    exec(&rt, "QUEUE PUSH qdelay_pop 'late' DELAY 5s");

    let pop = rt.execute_query("QUEUE POP qdelay_pop").expect("pop");
    assert!(
        pop.result.records.is_empty(),
        "delayed message must not be POP-able before due, got {:?}",
        pop.result.records
    );
}

#[test]
fn immediate_message_is_still_deliverable_alongside_delayed_one() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_mix");
    exec(&rt, "QUEUE GROUP CREATE qdelay_mix workers");
    exec(&rt, "QUEUE PUSH qdelay_mix 'now'");
    exec(&rt, "QUEUE PUSH qdelay_mix 'later' DELAY 10s");

    let read = rt
        .execute_query("QUEUE READ qdelay_mix GROUP workers CONSUMER c1 COUNT 10")
        .expect("read");
    assert_eq!(
        read.result.records.len(),
        1,
        "only the immediate message should be delivered, got {:?}",
        read.result.records
    );
}

#[test]
fn delayed_message_becomes_deliverable_after_due() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_due");
    exec(&rt, "QUEUE GROUP CREATE qdelay_due workers");
    exec(&rt, "QUEUE PUSH qdelay_due 'soon' DELAY 150ms");

    // Before due — empty.
    let read = rt
        .execute_query("QUEUE READ qdelay_due GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert!(read.result.records.is_empty());

    thread::sleep(Duration::from_millis(220));

    // After due — delivered.
    let read = rt
        .execute_query("QUEUE READ qdelay_due GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert_eq!(
        read.result.records.len(),
        1,
        "delayed message should be deliverable after its due time, got {:?}",
        read.result.records
    );
}

#[test]
fn absolute_available_at_in_the_past_delivers_immediately() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_past");
    exec(&rt, "QUEUE GROUP CREATE qdelay_past workers");
    // 1 ms after the unix epoch — well in the past.
    exec(&rt, "QUEUE PUSH qdelay_past 'historic' AVAILABLE AT 1");

    let read = rt
        .execute_query("QUEUE READ qdelay_past GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert_eq!(
        read.result.records.len(),
        1,
        "AVAILABLE AT in the past should deliver immediately, got {:?}",
        read.result.records
    );
}

#[test]
fn wait_wakes_when_delayed_message_becomes_due() {
    let rt = Arc::new(runtime());
    exec(&rt, "CREATE QUEUE qdelay_wait");
    exec(&rt, "QUEUE GROUP CREATE qdelay_wait workers");
    // Push a delayed message BEFORE the waiter parks.
    exec(&rt, "QUEUE PUSH qdelay_wait 'pending' DELAY 200ms");

    let started = Instant::now();
    let read = rt
        .execute_query("QUEUE READ qdelay_wait GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect("read");
    let elapsed = started.elapsed();

    assert_eq!(
        read.result.records.len(),
        1,
        "WAIT must deliver the delayed message once it becomes due, got {:?}",
        read.result.records
    );
    assert!(
        elapsed >= Duration::from_millis(180),
        "WAIT should not deliver before the message is due (elapsed={elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "WAIT should wake near the due time, not after the full budget (elapsed={elapsed:?})"
    );
}

#[test]
fn projection_exposes_available_at_for_delayed_message() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qdelay_proj");
    exec(&rt, "QUEUE PUSH qdelay_proj 'visible' DELAY 30s");

    // SELECT FROM QUEUE <name> exposes the inspection projection — the
    // row is present even though it is not yet deliverable.
    let select = rt
        .execute_query("SELECT id, available_at, enqueued_at FROM QUEUE qdelay_proj")
        .expect("select");
    assert_eq!(
        select.result.records.len(),
        1,
        "delayed message must be inspectable before its due time"
    );
    let record = &select.result.records[0];
    let enqueued_at = record_field(record, "enqueued_at");
    let available_at = record_field(record, "available_at");
    assert!(
        available_at > enqueued_at,
        "available_at ({available_at}) must be > enqueued_at ({enqueued_at}) for a delayed message"
    );
}

fn record_field(
    record: &reddb_server::storage::query::unified::UnifiedRecord,
    column: &str,
) -> u64 {
    let value = record
        .get(column)
        .unwrap_or_else(|| panic!("column '{column}' missing from record"));
    match value {
        Value::UnsignedInteger(v) => *v,
        Value::Integer(v) => *v as u64,
        other => panic!("expected unsigned integer for '{column}', got {other:?}"),
    }
}
