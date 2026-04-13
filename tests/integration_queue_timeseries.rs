use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::catalog::CollectionModel;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn text(record: &UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.clone(),
        Some(Value::UnsignedInteger(value)) => value.to_string(),
        other => panic!("expected text-like value for {column}, got {other:?}"),
    }
}

fn uint(record: &UnifiedRecord, column: &str) -> u64 {
    match record.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer for {column}, got {other:?}"),
    }
}

#[test]
fn test_queue_group_pending_claim_and_ack_flow() {
    let rt = rt();

    exec(
        &rt,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3",
    );
    exec(&rt, "QUEUE GROUP CREATE tasks workers");

    let pushed = exec(&rt, "QUEUE PUSH tasks 'job-1'");
    let message_id = text(&pushed.result.records[0], "message_id");

    let read = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    assert_eq!(
        read.result.records.len(),
        1,
        "read should return one message"
    );
    assert_eq!(text(&read.result.records[0], "message_id"), message_id);
    assert_eq!(text(&read.result.records[0], "payload"), "job-1");
    assert_eq!(text(&read.result.records[0], "consumer"), "worker1");

    let pending = exec(&rt, "QUEUE PENDING tasks GROUP workers");
    assert_eq!(
        pending.result.records.len(),
        1,
        "pending should list the delivered message"
    );
    assert_eq!(text(&pending.result.records[0], "consumer"), "worker1");

    let claimed = exec(
        &rt,
        "QUEUE CLAIM tasks GROUP workers CONSUMER worker2 MIN_IDLE 0",
    );
    assert_eq!(
        claimed.result.records.len(),
        1,
        "claim should transfer the pending message"
    );
    assert_eq!(text(&claimed.result.records[0], "message_id"), message_id);
    assert_eq!(text(&claimed.result.records[0], "consumer"), "worker2");

    let pending_after_claim = exec(&rt, "QUEUE PENDING tasks GROUP workers");
    assert_eq!(pending_after_claim.result.records.len(), 1);
    assert_eq!(
        text(&pending_after_claim.result.records[0], "consumer"),
        "worker2"
    );

    exec(
        &rt,
        &format!("QUEUE ACK tasks GROUP workers '{}'", message_id),
    );

    let pending_after_ack = exec(&rt, "QUEUE PENDING tasks GROUP workers");
    assert!(
        pending_after_ack.result.records.is_empty(),
        "ack should clear pending entries"
    );

    let len = exec(&rt, "QUEUE LEN tasks");
    assert_eq!(
        uint(&len.result.records[0], "len"),
        0,
        "single-group ack should remove the message from the queue"
    );
}

#[test]
fn test_queue_nack_moves_message_to_dlq_after_max_attempts() {
    let rt = rt();

    exec(
        &rt,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 2",
    );
    exec(&rt, "QUEUE GROUP CREATE tasks workers");
    exec(&rt, "QUEUE PUSH tasks 'job-dlq'");

    let first_read = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    let message_id = text(&first_read.result.records[0], "message_id");
    exec(
        &rt,
        &format!("QUEUE NACK tasks GROUP workers '{}'", message_id),
    );

    let second_read = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    assert_eq!(second_read.result.records.len(), 1);
    assert_eq!(
        text(&second_read.result.records[0], "message_id"),
        message_id
    );
    let moved = exec(
        &rt,
        &format!("QUEUE NACK tasks GROUP workers '{}'", message_id),
    );
    assert!(
        moved.query.contains("QUEUE NACK"),
        "nack command should complete after moving to DLQ"
    );

    let main_len = exec(&rt, "QUEUE LEN tasks");
    assert_eq!(uint(&main_len.result.records[0], "len"), 0);

    let dlq_len = exec(&rt, "QUEUE LEN failed_tasks");
    assert_eq!(uint(&dlq_len.result.records[0], "len"), 1);

    let dlq_peek = exec(&rt, "QUEUE PEEK failed_tasks");
    assert_eq!(dlq_peek.result.records.len(), 1);
    assert_eq!(text(&dlq_peek.result.records[0], "payload"), "job-dlq");
}

#[test]
fn test_create_timeseries_persists_contract_and_downsample_metadata() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TIMESERIES cpu_metrics RETENTION 90 d DOWNSAMPLE 1h:5m:avg, 1d:1h:max",
    );

    let contract = rt
        .db()
        .collection_contract("cpu_metrics")
        .expect("timeseries contract should exist");
    assert_eq!(contract.declared_model, CollectionModel::TimeSeries);
    assert_eq!(contract.default_ttl_ms, Some(90 * 86_400_000));

    let store = rt.db().store();
    let meta = store
        .get_collection("red_timeseries_meta")
        .expect("timeseries metadata collection should exist");
    let rows = meta.query_all(|entity| {
        entity
            .data
            .as_row()
            .is_some_and(|row| match row.get_field("series") {
                Some(Value::Text(series)) => series == "cpu_metrics",
                _ => false,
            })
    });
    assert_eq!(rows.len(), 1, "timeseries config row should be persisted");

    let row = rows[0]
        .data
        .as_row()
        .expect("metadata row should be a table row");
    match row.get_field("downsample_policies") {
        Some(Value::Array(values)) => {
            let rendered = values
                .iter()
                .map(|value| match value {
                    Value::Text(text) => text.clone(),
                    other => panic!("unexpected policy value: {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(rendered, vec!["1h:5m:avg", "1d:1h:max"]);
        }
        other => panic!("expected downsample policy array, got {other:?}"),
    }
}
