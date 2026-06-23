//! Slice 10 of issue #527 — QueueLifecycle telemetry.
//!
//! Drives a NACK→DLQ scenario through the user-facing
//! `QUEUE PUSH / READ / NACK` surface and asserts both the
//! Prometheus exposition (`queue_*_total`, `queue_pending_gauge`)
//! and the audit log (`operator/queue_dlq_promoted`) reflect the
//! promotion.

#[path = "../../support/mod.rs"]
mod support;

use std::time::Duration;

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::storage::layout::{LayoutOverrides, LogDestination, LogRoutingOverrides};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

use support::prometheus::get;

fn rt(tag: &str) -> (support::TempDbFile, RedDBRuntime) {
    let db = support::temp_db_file(tag);
    let options = RedDBOptions::persistent(db.path()).with_layout_overrides(LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(LogDestination::File(db.path().with_file_name("audit.log"))),
            ..LogRoutingOverrides::default()
        },
        ..LayoutOverrides::default()
    });
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    (db, runtime)
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn message_id_of(result: &reddb::runtime::RuntimeQueryResult) -> String {
    match result.result.records[0].get("message_id") {
        Some(Value::Text(value)) => value.to_string(),
        Some(Value::UnsignedInteger(value)) => value.to_string(),
        other => panic!("unexpected message_id: {other:?}"),
    }
}

#[test]
fn nack_to_dlq_emits_audit_event_and_increments_prom_counters() {
    // The operator event sink is process-global and first-runtime-wins.
    // Queue DLQ promotion must still persist to the runtime that performed
    // the NACK, not whichever runtime first installed the global sink.
    let _global_sink_owner = rt("queue-lifecycle-global-sink");
    let (_guard, rt) = rt("queue-lifecycle-audit");

    // Build a queue with a DLQ target + a low max_attempts so the
    // second NACK promotes immediately.
    exec(
        &rt,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 4",
    );
    exec(&rt, "QUEUE GROUP CREATE tasks workers");
    exec(&rt, "QUEUE PUSH tasks 'job-dlq'");

    // Read 1 + NACK 1 → requeue (retry outcome).
    let first = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    let message_id = message_id_of(&first);
    exec(
        &rt,
        &format!("QUEUE NACK tasks GROUP workers '{}'", message_id),
    );

    // Read 2 + NACK 2 → DLQ promotion.
    let _ = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    exec(
        &rt,
        &format!("QUEUE NACK tasks GROUP workers '{}'", message_id),
    );

    // -------------------------------------------------------------
    // Audit log assertion — DLQ promotion is forensic, must persist.
    // -------------------------------------------------------------
    let audit_path = rt.audit_log().path().to_path_buf();
    assert!(
        rt.audit_log().wait_idle(Duration::from_secs(2)),
        "audit logger drain timed out"
    );
    let audit_body = std::fs::read_to_string(&audit_path)
        .unwrap_or_else(|err| panic!("audit log read at {audit_path:?}: {err}"));
    let promotion_line = audit_body
        .lines()
        .find(|line| {
            line.contains("operator/queue_dlq_promoted")
                && line.contains("\"queue\":\"tasks\"")
                && line.contains("\"group\":\"workers\"")
                && line.contains("\"dlq\":\"failed_tasks\"")
        })
        .unwrap_or_else(|| {
            panic!("audit log did not include operator/queue_dlq_promoted. body:\n{audit_body}")
        });
    assert!(
        promotion_line.contains("\"queue\":\"tasks\""),
        "audit line missing queue field: {promotion_line}"
    );
    assert!(
        promotion_line.contains("\"group\":\"workers\""),
        "audit line missing group field: {promotion_line}"
    );
    assert!(
        promotion_line.contains("\"dlq\":\"failed_tasks\""),
        "audit line missing dlq field: {promotion_line}"
    );

    // -------------------------------------------------------------
    // Prometheus assertion — scrape via the public /metrics endpoint
    // -------------------------------------------------------------
    let (status, body) = get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "metrics endpoint should return 200");

    // deliver fired twice (one per READ).
    assert!(
        body.contains("queue_delivered_total{queue=\"tasks\",group=\"workers\",mode=\"work\"} 2"),
        "missing delivered counter line. body:\n{body}"
    );
    // First NACK was a retry.
    assert!(
        body.contains(
            "queue_nacked_total{queue=\"tasks\",group=\"workers\",mode=\"work\",outcome=\"retry\"} 1"
        ),
        "missing retry-nack counter line. body:\n{body}"
    );
    // Second NACK promoted to DLQ.
    assert!(
        body.contains(
            "queue_nacked_total{queue=\"tasks\",group=\"workers\",mode=\"work\",outcome=\"dlq\"} 1"
        ),
        "missing dlq-nack counter line. body:\n{body}"
    );
    // After both NACKs the pending row is gone, so the gauge
    // either reports 0 for the (tasks, workers) pair or omits it.
    if let Some(line) = body
        .lines()
        .find(|line| line.starts_with("queue_pending_gauge{queue=\"tasks\",group=\"workers\"}"))
    {
        assert!(
            line.trim_end().ends_with(" 0"),
            "expected pending gauge to be zero, got: {line}"
        );
    }
}
