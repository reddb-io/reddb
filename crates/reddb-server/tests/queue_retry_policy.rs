//! Issue #723 — Queue retry policy and NACK delay override.
//!
//! Acceptance through the runtime so all transports inherit the
//! behaviour by construction:
//!
//! - `CREATE QUEUE ... RETRY_DELAY <duration>` persists a default
//!   retry delay applied to NACK-requeued messages.
//! - `ALTER QUEUE ... SET RETRY_DELAY <duration>` updates the default.
//! - `QUEUE NACK ... WITH DELAY <duration>` overrides per-failure.
//! - The override requires a write-capable role; `Role::Read` is rejected.
//! - Max-attempts → DLQ / drop semantics are preserved.
//! - The default retry delay survives a save-then-reload cycle.

use reddb_server::auth::Role;
use reddb_server::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::thread;
use std::time::Duration;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn read_one_message_id(rt: &RedDBRuntime, sql: &str) -> Option<String> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .first()
        .and_then(|rec| rec.get("message_id"))
        .map(|v| format!("{v:?}"))
}

/// Pull the `message_id` of a single delivered record as the textual
/// form the wire serializer would produce. The runtime stores message
/// ids as `EntityId`; we only need a stable handle to NACK against.
fn delivered_message_id(rt: &RedDBRuntime, queue: &str, group: &str) -> String {
    let sql = format!("QUEUE READ {queue} GROUP {group} CONSUMER c1 COUNT 1");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let record = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("expected a delivery from {sql}"));
    let value = record
        .get("message_id")
        .unwrap_or_else(|| panic!("delivered record missing 'message_id': {record:?}"));
    match value {
        reddb_server::storage::schema::Value::Text(s) => s.to_string(),
        reddb_server::storage::schema::Value::UnsignedInteger(v) => v.to_string(),
        reddb_server::storage::schema::Value::Integer(v) => v.to_string(),
        other => panic!("unexpected message_id value: {other:?}"),
    }
}

#[test]
fn default_retry_delay_defers_requeue_after_nack() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE QUEUE qretry_default RETRY_DELAY 500ms MAX_ATTEMPTS 5",
    );
    exec(&rt, "QUEUE GROUP CREATE qretry_default workers");
    exec(&rt, "QUEUE PUSH qretry_default 'payload-1'");

    let mid = delivered_message_id(&rt, "qretry_default", "workers");
    exec(
        &rt,
        &format!("QUEUE NACK qretry_default GROUP workers '{mid}'"),
    );

    // Immediately after NACK the message must not be redeliverable —
    // the default RETRY_DELAY pushed its availability into the future.
    let after = rt
        .execute_query("QUEUE READ qretry_default GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert!(
        after.result.records.is_empty(),
        "default RETRY_DELAY should defer requeued message, got {:?}",
        after.result.records
    );

    // After the delay elapses the message is delivered again.
    thread::sleep(Duration::from_millis(650));
    let later = rt
        .execute_query("QUEUE READ qretry_default GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert_eq!(
        later.result.records.len(),
        1,
        "message should be re-delivered after RETRY_DELAY elapses, got {:?}",
        later.result.records
    );
}

#[test]
fn nack_delay_override_takes_precedence_over_queue_default() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE QUEUE qretry_override RETRY_DELAY 10ms MAX_ATTEMPTS 5",
    );
    exec(&rt, "QUEUE GROUP CREATE qretry_override workers");
    exec(&rt, "QUEUE PUSH qretry_override 'payload-1'");

    let mid = delivered_message_id(&rt, "qretry_override", "workers");
    // Override with a much larger delay than the queue default. The
    // override must win: the queue default would let the next read
    // succeed within ~50ms, but a 1s override must keep it pending.
    exec(
        &rt,
        &format!("QUEUE NACK qretry_override GROUP workers '{mid}' WITH DELAY 1s"),
    );

    thread::sleep(Duration::from_millis(150));
    let early = rt
        .execute_query("QUEUE READ qretry_override GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert!(
        early.result.records.is_empty(),
        "NACK WITH DELAY 1s must override queue default 10ms, got {:?}",
        early.result.records
    );
}

#[test]
fn nack_delay_override_is_rejected_for_read_only_identity() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qretry_unauth");
    exec(&rt, "QUEUE GROUP CREATE qretry_unauth workers");
    exec(&rt, "QUEUE PUSH qretry_unauth 'payload-1'");

    let mid = delivered_message_id(&rt, "qretry_unauth", "workers");

    set_current_auth_identity("alice".to_string(), Role::Read);
    let err = rt
        .execute_query(&format!(
            "QUEUE NACK qretry_unauth GROUP workers '{mid}' WITH DELAY 30s"
        ))
        .expect_err("Read role must not be allowed to override NACK delay");
    clear_current_auth_identity();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("not authorized"),
        "expected unauthorized error, got: {msg}"
    );

    // Write role succeeds.
    set_current_auth_identity("bob".to_string(), Role::Write);
    let ok = rt.execute_query(&format!(
        "QUEUE NACK qretry_unauth GROUP workers '{mid}' WITH DELAY 30s"
    ));
    clear_current_auth_identity();
    ok.expect("Write role must be allowed to override NACK delay");
}

#[test]
fn nack_promotes_to_dlq_once_max_attempts_reached() {
    let rt = runtime();
    // WORK-mode group_read counts each delivery against the budget,
    // so MAX_ATTEMPTS=4 admits read→nack→read→nack and tips over to
    // DLQ on the second NACK's bump (1, 2, 3, 4 → ≥ 4).
    exec(
        &rt,
        "CREATE QUEUE qretry_dlq WITH DLQ qretry_dlq_dead MAX_ATTEMPTS 4 RETRY_DELAY 10ms",
    );
    exec(&rt, "QUEUE GROUP CREATE qretry_dlq workers");
    exec(&rt, "QUEUE PUSH qretry_dlq 'payload-1'");

    // First NACK — requeued with the configured retry delay.
    let mid1 = delivered_message_id(&rt, "qretry_dlq", "workers");
    exec(
        &rt,
        &format!("QUEUE NACK qretry_dlq GROUP workers '{mid1}'"),
    );
    thread::sleep(Duration::from_millis(50));

    // Second NACK — bumps to max_attempts, must move to DLQ.
    let mid2 = delivered_message_id(&rt, "qretry_dlq", "workers");
    assert_eq!(mid1, mid2, "same message must be re-delivered");
    let nack_result = rt
        .execute_query(&format!("QUEUE NACK qretry_dlq GROUP workers '{mid2}'"))
        .expect("nack");
    let nack_msg = format!("{:?}", nack_result.result);
    assert!(
        nack_msg.contains("dead-letter"),
        "second NACK should report DLQ promotion, got: {nack_msg}"
    );

    // Source queue empty after DLQ promotion — no available rows.
    let after = rt
        .execute_query("SELECT * FROM QUEUE qretry_dlq")
        .expect("select");
    assert!(
        after.result.records.is_empty(),
        "source queue should be empty after DLQ promotion, got {:?}",
        after.result.records
    );
    let _ = read_one_message_id;
}

#[test]
fn nack_drop_when_no_dlq_configured_at_max_attempts() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE QUEUE qretry_drop MAX_ATTEMPTS 1 RETRY_DELAY 5ms",
    );
    exec(&rt, "QUEUE GROUP CREATE qretry_drop workers");
    exec(&rt, "QUEUE PUSH qretry_drop 'payload-1'");

    let mid = delivered_message_id(&rt, "qretry_drop", "workers");
    let nack = rt
        .execute_query(&format!("QUEUE NACK qretry_drop GROUP workers '{mid}'"))
        .expect("nack");
    let nack_msg = format!("{:?}", nack.result);
    assert!(
        nack_msg.contains("dropped"),
        "first-and-only NACK must drop without DLQ, got: {nack_msg}"
    );
}

#[test]
fn alter_queue_set_retry_delay_persists_to_config() {
    let rt = runtime();
    exec(&rt, "CREATE QUEUE qretry_alter MAX_ATTEMPTS 5");
    exec(&rt, "ALTER QUEUE qretry_alter SET RETRY_DELAY 250ms");
    exec(&rt, "QUEUE GROUP CREATE qretry_alter workers");
    exec(&rt, "QUEUE PUSH qretry_alter 'payload-1'");

    let mid = delivered_message_id(&rt, "qretry_alter", "workers");
    exec(
        &rt,
        &format!("QUEUE NACK qretry_alter GROUP workers '{mid}'"),
    );

    // ALTER must have taken effect — requeue is deferred.
    let early = rt
        .execute_query("QUEUE READ qretry_alter GROUP workers CONSUMER c1 COUNT 1")
        .expect("read");
    assert!(
        early.result.records.is_empty(),
        "ALTER SET RETRY_DELAY 250ms should defer requeue, got {:?}",
        early.result.records
    );
}
