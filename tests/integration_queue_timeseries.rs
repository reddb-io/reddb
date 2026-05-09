mod support;

use std::thread::sleep;
use std::time::Duration;

use reddb::application::{CatalogUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::catalog::CollectionModel;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::queue::QueueMode;
use reddb::storage::schema::Value;
use reddb::storage::{
    EntityData, EntityId, EntityKind, TimeSeriesData, TimeSeriesPointKind, UnifiedEntity,
};
use reddb::RedDBRuntime;
use std::collections::{HashMap, HashSet};

use support::{checkpoint_and_reopen, PersistentDbPath};

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
        Some(Value::Text(value)) => value.to_string(),
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

fn float(record: &UnifiedRecord, column: &str) -> f64 {
    match record.get(column) {
        Some(Value::Float(value)) => *value,
        Some(Value::Integer(value)) => *value as f64,
        Some(Value::UnsignedInteger(value)) => *value as f64,
        other => panic!("expected numeric value for {column}, got {other:?}"),
    }
}

fn payloads(result: &reddb::runtime::RuntimeQueryResult) -> Vec<String> {
    result
        .result
        .records
        .iter()
        .map(|record| match record.get("payload") {
            Some(Value::Text(value)) => value.to_string(),
            Some(Value::Json(bytes)) => {
                let json: reddb::json::Value =
                    reddb::json::from_slice(bytes).expect("payload json should decode");
                json.to_string()
            }
            other => panic!("expected payload value, got {other:?}"),
        })
        .collect()
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
fn test_queue_aliases_preserve_deque_sides() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE tasks");
    exec(&rt, "QUEUE RPUSH tasks 'tail-a'");
    exec(&rt, "QUEUE LPUSH tasks 'head'");
    exec(&rt, "QUEUE RPUSH tasks 'tail-b'");

    let peek = exec(&rt, "QUEUE PEEK tasks 3");
    assert_eq!(payloads(&peek), vec!["head", "tail-a", "tail-b"]);

    let left = exec(&rt, "QUEUE LPOP tasks");
    assert_eq!(payloads(&left), vec!["head"]);

    let right = exec(&rt, "QUEUE RPOP tasks");
    assert_eq!(payloads(&right), vec!["tail-b"]);
}

#[test]
fn test_queue_push_accepts_inline_json_literal() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE tasks");
    exec(&rt, "QUEUE PUSH tasks {job: 'process', retries: 3}");

    let peek = exec(&rt, "QUEUE PEEK tasks");
    match peek.result.records[0].get("payload") {
        Some(Value::Json(bytes)) => {
            let json: reddb::json::Value =
                reddb::json::from_slice(bytes).expect("payload json should decode");
            assert_eq!(
                json.get("job").and_then(reddb::json::Value::as_str),
                Some("process")
            );
            assert_eq!(
                json.get("retries").and_then(reddb::json::Value::as_i64),
                Some(3)
            );
        }
        other => panic!("expected json payload, got {other:?}"),
    }
}

#[test]
fn test_queue_ttl_expires_messages_after_retention() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE tasks WITH TTL 1 ms");
    exec(&rt, "QUEUE PUSH tasks 'ttl-test'");
    sleep(Duration::from_millis(10));

    rt.apply_retention_policy()
        .expect("queue retention policy should succeed");

    let len = exec(&rt, "QUEUE LEN tasks");
    assert_eq!(uint(&len.result.records[0], "len"), 0);
}

#[test]
fn test_queue_persistent_reopen_retains_messages_and_recovers_pending() {
    let path = PersistentDbPath::new("queue_reopen");
    let rt = path.open_runtime();

    exec(
        &rt,
        "CREATE QUEUE tasks PRIORITY WITH DLQ failed_tasks MAX_ATTEMPTS 5",
    );
    exec(&rt, "QUEUE GROUP CREATE tasks workers");
    exec(&rt, "QUEUE PUSH tasks 'job-1' PRIORITY 10");

    let first_read = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );
    assert_eq!(payloads(&first_read), vec!["job-1"]);

    let rt = checkpoint_and_reopen(&path, rt);

    let store = rt.db().store();
    let tasks = store
        .get_collection("tasks")
        .expect("tasks queue collection should exist after reopen");
    let messages = tasks.query_all(|entity| matches!(entity.kind, EntityKind::QueueMessage { .. }));
    assert_eq!(messages.len(), 1, "queue message should survive reopen");
    match &messages[0].data {
        EntityData::QueueMessage(message) => {
            assert_eq!(message.priority, Some(10));
            assert_eq!(message.attempts, 1);
            assert_eq!(message.max_attempts, 5);
        }
        other => panic!("expected queue message entity, got {other:?}"),
    }

    let len = exec(&rt, "QUEUE LEN tasks");
    assert_eq!(uint(&len.result.records[0], "len"), 1);

    let recovered = exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker2 COUNT 1",
    );
    assert_eq!(payloads(&recovered), vec!["job-1"]);
}

#[test]
fn test_queue_mode_persists_and_defaults_to_work() {
    let path = PersistentDbPath::new("queue_mode_catalog");
    let rt = path.open_runtime();

    exec(&rt, "CREATE QUEUE fanout_tasks FANOUT");
    exec(&rt, "CREATE QUEUE work_tasks WORK");
    exec(&rt, "CREATE QUEUE default_tasks");
    exec(&rt, "ALTER QUEUE default_tasks SET MODE FANOUT");

    let rt = checkpoint_and_reopen(&path, rt);
    let catalog = CatalogUseCases::new(&rt).snapshot();

    let mode = |name: &str| {
        catalog
            .collections
            .iter()
            .find(|collection| collection.name == name)
            .and_then(|collection| collection.queue_mode)
    };
    assert_eq!(mode("fanout_tasks"), Some(QueueMode::Fanout));
    assert_eq!(mode("work_tasks"), Some(QueueMode::Work));
    assert_eq!(mode("default_tasks"), Some(QueueMode::Fanout));
}

// Conformance: WORK→FANOUT mode transition.
// In-flight messages (pending before ALTER) drain with old WORK mode.
// New messages pushed after ALTER are delivered with FANOUT semantics.
#[test]
fn test_alter_queue_work_to_fanout_transition() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE alerts WORK");

    // Push 2 messages before the mode switch.
    exec(&rt, "QUEUE PUSH alerts 'pre-1'");
    exec(&rt, "QUEUE PUSH alerts 'pre-2'");

    // Consumer alice reads one message in WORK mode — it lands in _work_default pending.
    let read = exec(&rt, "QUEUE READ alerts CONSUMER alice COUNT 1");
    assert_eq!(read.result.records.len(), 1, "alice should get 1 message");
    let pre_msg_id = text(&read.result.records[0], "message_id");

    // ALTER: switch to FANOUT. In-flight message for alice is still pending (old mode).
    let alter = exec(&rt, "ALTER QUEUE alerts SET MODE FANOUT");
    assert!(
        text(&alter.result.records[0], "message").contains("fanout"),
        "result should confirm new mode"
    );

    // Push a new message after the mode switch.
    exec(&rt, "QUEUE PUSH alerts 'post-1'");

    // Bob reads — should get both the remaining pre-2 AND post-1 in FANOUT semantics
    // (each fanout consumer gets every message not yet acked for their group).
    let bob_reads: Vec<_> = {
        let mut msgs = Vec::new();
        loop {
            let r = exec(&rt, "QUEUE READ alerts CONSUMER bob COUNT 10");
            if r.result.records.is_empty() {
                break;
            }
            for rec in &r.result.records {
                msgs.push(text(rec, "payload"));
            }
        }
        msgs
    };
    assert!(
        bob_reads.contains(&"pre-2".to_string()),
        "bob should get pre-2 (not yet acked) after FANOUT switch"
    );
    assert!(
        bob_reads.contains(&"post-1".to_string()),
        "bob should get post-1 pushed after FANOUT switch"
    );

    // Alice can still ack her in-flight message from the old WORK pending.
    exec(
        &rt,
        &format!("QUEUE ACK alerts GROUP _work_default '{pre_msg_id}'"),
    );
}

#[test]
fn test_work_queue_implicit_consumers_share_default_group() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE work_tasks WORK");
    for i in 0..100 {
        exec(&rt, &format!("QUEUE PUSH work_tasks 'job-{i}'"));
    }

    let consumers = ["alice", "bob", "carol"];
    let mut counts = HashMap::new();
    let mut seen = HashSet::new();

    for i in 0.. {
        let consumer = consumers[i % consumers.len()];
        let read = exec(
            &rt,
            &format!("QUEUE READ work_tasks CONSUMER {consumer} COUNT 1"),
        );
        if read.result.records.is_empty() {
            break;
        }

        assert_eq!(read.result.records.len(), 1);
        assert_eq!(text(&read.result.records[0], "consumer"), consumer);
        let message_id = text(&read.result.records[0], "message_id");
        assert!(
            seen.insert(message_id),
            "WORK implicit consumers must not overlap messages"
        );
        *counts.entry(consumer.to_string()).or_insert(0usize) += 1;
    }

    assert_eq!(seen.len(), 100);
    assert_eq!(counts.get("alice"), Some(&34));
    assert_eq!(counts.get("bob"), Some(&33));
    assert_eq!(counts.get("carol"), Some(&33));

    let pending = exec(&rt, "QUEUE PENDING work_tasks GROUP _work_default");
    assert_eq!(pending.result.records.len(), 100);
}

// Conformance: FANOUT semantics — each consumer receives every message independently.
#[test]
fn test_fanout_queue_broadcast_all_consumers_get_all_messages() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE notifications FANOUT");
    for i in 0..100 {
        exec(&rt, &format!("QUEUE PUSH notifications 'msg-{i}'"));
    }

    let consumers = ["alice", "bob", "carol"];
    let mut per_consumer_ids: HashMap<&str, HashSet<String>> = HashMap::new();

    for consumer in &consumers {
        let mut seen = HashSet::new();
        loop {
            let read = exec(
                &rt,
                &format!("QUEUE READ notifications CONSUMER {consumer} COUNT 10"),
            );
            if read.result.records.is_empty() {
                break;
            }
            for record in &read.result.records {
                let msg_id = text(record, "message_id");
                let fanout_group = format!("_fanout_{consumer}");
                exec(
                    &rt,
                    &format!("QUEUE ACK notifications GROUP {fanout_group} '{msg_id}'"),
                );
                seen.insert(text(record, "payload"));
            }
        }
        per_consumer_ids.insert(consumer, seen);
    }

    // Every consumer must have received all 100 messages.
    for consumer in &consumers {
        assert_eq!(
            per_consumer_ids[consumer].len(),
            100,
            "consumer {consumer} should receive all 100 messages in FANOUT mode"
        );
    }
}

#[test]
fn test_fanout_queue_ack_isolation() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE events FANOUT");
    exec(&rt, "QUEUE PUSH events 'hello'");

    // alice reads and acks
    let read_alice = exec(&rt, "QUEUE READ events CONSUMER alice COUNT 1");
    assert_eq!(read_alice.result.records.len(), 1);
    let msg_id = text(&read_alice.result.records[0], "message_id");
    exec(
        &rt,
        &format!("QUEUE ACK events GROUP _fanout_alice '{msg_id}'"),
    );

    // bob has not yet read — message must still be visible to bob
    let read_bob = exec(&rt, "QUEUE READ events CONSUMER bob COUNT 1");
    assert_eq!(
        read_bob.result.records.len(),
        1,
        "bob must still see the message after alice acked it"
    );
    let bob_msg_id = text(&read_bob.result.records[0], "message_id");
    assert_eq!(msg_id, bob_msg_id, "alice and bob see the same message");
}

#[test]
fn test_fanout_queue_dlq_per_consumer() {
    let rt = rt();

    // max_attempts=1 so a single nack triggers DLQ
    exec(
        &rt,
        "CREATE QUEUE alerts FANOUT WITH DLQ alerts_dlq MAX_ATTEMPTS 1",
    );
    exec(&rt, "QUEUE PUSH alerts 'critical'");

    // alice reads and nacks — should move to alice's DLQ (group-level ack),
    // but bob must still be able to read the message.
    let read_alice = exec(&rt, "QUEUE READ alerts CONSUMER alice COUNT 1");
    assert_eq!(read_alice.result.records.len(), 1);
    let msg_id = text(&read_alice.result.records[0], "message_id");
    // delivery_count=1, max_attempts=1 → nack triggers DLQ
    exec(
        &rt,
        &format!("QUEUE NACK alerts GROUP _fanout_alice '{msg_id}'"),
    );

    // bob must still receive the message
    let read_bob = exec(&rt, "QUEUE READ alerts CONSUMER bob COUNT 1");
    assert_eq!(
        read_bob.result.records.len(),
        1,
        "bob must not be affected by alice's DLQ move"
    );
}

#[test]
fn test_timeseries_persistent_reopen_retains_tags() {
    let path = PersistentDbPath::new("timeseries_reopen_tags");
    let rt = path.open_runtime();

    exec(&rt, "CREATE TIMESERIES cpu_metrics RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu.idle', 94.8, {host: 'srv1', region: 'us-east'}, 1704067200000000000)",
    );

    let rt = checkpoint_and_reopen(&path, rt);

    let selected = exec(
        &rt,
        "SELECT metric, value, timestamp, tags FROM cpu_metrics WHERE metric = 'cpu.idle'",
    );
    assert_eq!(selected.result.records.len(), 1);
    assert_eq!(text(&selected.result.records[0], "metric"), "cpu.idle");
    assert_eq!(
        uint(&selected.result.records[0], "timestamp"),
        1_704_067_200_000_000_000u64
    );
    match selected.result.records[0].get("tags") {
        Some(Value::Json(bytes)) => {
            let json: reddb::json::Value =
                reddb::json::from_slice(bytes).expect("tags json should decode after reopen");
            assert_eq!(
                json.get("host").and_then(reddb::json::Value::as_str),
                Some("srv1")
            );
            assert_eq!(
                json.get("region").and_then(reddb::json::Value::as_str),
                Some("us-east")
            );
        }
        other => panic!("expected json tags after reopen, got {other:?}"),
    }

    let store = rt.db().store();
    let manager = store
        .get_collection("cpu_metrics")
        .expect("cpu_metrics collection should exist after reopen");
    let entities = manager.query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
    assert_eq!(entities.len(), 1);
    match &entities[0].data {
        EntityData::TimeSeries(ts) => {
            assert_eq!(ts.tags.get("host").map(String::as_str), Some("srv1"));
            assert_eq!(ts.tags.get("region").map(String::as_str), Some("us-east"));
        }
        other => panic!("expected native timeseries entity, got {other:?}"),
    }
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
                Some(Value::Text(series)) => series.as_ref() == "cpu_metrics",
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
                    Value::Text(text) => text.to_string(),
                    other => panic!("unexpected policy value: {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                rendered,
                vec!["1h:5m:avg".to_string(), "1d:1h:max".to_string()]
            );
        }
        other => panic!("expected downsample policy array, got {other:?}"),
    }
}

#[test]
fn test_insert_into_timeseries_uses_native_point_entities() {
    let rt = rt();

    exec(&rt, "CREATE TIMESERIES cpu_metrics RETENTION 7 d");

    let explicit_timestamp = 1_704_067_200_000_000_000u64;
    exec(
        &rt,
        "INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu.idle', 94.8, {host: 'srv1', region: 'us-east'}, 1704067200000000000)",
    );
    exec(
        &rt,
        "INSERT INTO cpu_metrics (metric, value, tags) VALUES ('cpu.idle', 95.2, '{\"host\":\"srv2\"}')",
    );

    let selected = exec(
        &rt,
        "SELECT metric, value, timestamp, tags FROM cpu_metrics WHERE metric = 'cpu.idle' ORDER BY timestamp ASC",
    );
    assert_eq!(selected.result.records.len(), 2);
    assert_eq!(text(&selected.result.records[0], "metric"), "cpu.idle");
    assert_eq!(
        uint(&selected.result.records[0], "timestamp"),
        explicit_timestamp
    );
    match selected.result.records[0].get("tags") {
        Some(Value::Json(bytes)) => {
            let json: reddb::json::Value =
                reddb::json::from_slice(bytes).expect("tags json should decode");
            assert_eq!(
                json.get("host").and_then(reddb::json::Value::as_str),
                Some("srv1")
            );
            assert_eq!(
                json.get("region").and_then(reddb::json::Value::as_str),
                Some("us-east")
            );
        }
        other => panic!("expected json tags in query result, got {other:?}"),
    }
    assert!(
        uint(&selected.result.records[1], "timestamp") > explicit_timestamp,
        "implicit timestamp should be generated at insert time"
    );

    let tag_filtered = exec(
        &rt,
        "SELECT metric, value, tags FROM cpu_metrics WHERE tags.host = 'srv1' ORDER BY timestamp ASC",
    );
    assert_eq!(tag_filtered.result.records.len(), 1);
    assert_eq!(text(&tag_filtered.result.records[0], "metric"), "cpu.idle");
    assert!((float(&tag_filtered.result.records[0], "value") - 94.8).abs() < 0.0001);

    let store = rt.db().store();
    let manager = store
        .get_collection("cpu_metrics")
        .expect("cpu_metrics collection should exist");
    let mut entities = manager.query_all(|_| true);
    assert_eq!(entities.len(), 2);
    entities.sort_by_key(|entity| entity.id.raw());
    assert!(entities
        .iter()
        .all(|entity| matches!(entity.data, EntityData::TimeSeries(_))));

    match &entities[0].data {
        EntityData::TimeSeries(ts) => {
            assert_eq!(ts.metric, "cpu.idle");
            assert_eq!(ts.timestamp_ns, explicit_timestamp);
            assert_eq!(ts.value, 94.8);
            assert_eq!(ts.tags.get("host").map(String::as_str), Some("srv1"));
            assert_eq!(ts.tags.get("region").map(String::as_str), Some("us-east"));
        }
        other => panic!("expected native timeseries entity, got {other:?}"),
    }
}

#[test]
fn test_timeseries_time_bucket_aggregate_query() {
    let rt = rt();

    exec(&rt, "CREATE TIMESERIES cpu_metrics RETENTION 7 d");

    let store = rt.db().store();
    let five_minutes_ns = 300_000_000_000u64;
    let samples = [(0, 10.0), (60_000_000_000, 20.0), (five_minutes_ns, 30.0)];

    for (timestamp_ns, value) in samples {
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TimeSeriesPoint(Box::new(TimeSeriesPointKind {
                series: "cpu_metrics".to_string(),
                metric: "cpu.usage".to_string(),
            })),
            EntityData::TimeSeries(TimeSeriesData {
                metric: "cpu.usage".to_string(),
                timestamp_ns,
                value,
                tags: HashMap::new(),
            }),
        );
        store
            .insert_auto("cpu_metrics", entity)
            .expect("timeseries sample insert should succeed");
    }

    let filtered = exec(
        &rt,
        "SELECT metric, value FROM cpu_metrics WHERE metric = 'cpu.usage'",
    );
    assert_eq!(
        filtered.result.records.len(),
        3,
        "plain filtered select should include timeseries points"
    );

    let result = exec(
        &rt,
        "SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples FROM cpu_metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)",
    );

    assert_eq!(result.result.records.len(), 2, "expected two time buckets");

    let mut rows: Vec<(u64, f64, u64)> = result
        .result
        .records
        .iter()
        .map(|record| {
            (
                uint(record, "bucket"),
                float(record, "avg_value"),
                uint(record, "samples"),
            )
        })
        .collect();
    rows.sort_by_key(|(bucket, _, _)| *bucket);

    assert_eq!(rows[0], (0, 15.0, 2));
    assert_eq!(rows[1], (five_minutes_ns, 30.0, 1));
}

#[test]
fn test_queue_mode_visible_in_red_collections() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE notifications FANOUT");
    exec(&rt, "CREATE QUEUE tasks WORK");

    let result = exec(
        &rt,
        "SELECT name, queue_mode FROM red.collections WHERE model = 'queue'",
    );

    let queue_modes: std::collections::HashMap<String, String> = result
        .result
        .records
        .iter()
        .filter(|r| r.get("queue_mode") != Some(&Value::Null))
        .map(|r| (text(r, "name"), text(r, "queue_mode")))
        .collect();

    assert_eq!(queue_modes.get("notifications").map(String::as_str), Some("fanout"));
    assert_eq!(queue_modes.get("tasks").map(String::as_str), Some("work"));
}

#[test]
fn test_non_queue_collections_have_null_queue_mode() {
    let rt = rt();

    exec(&rt, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");

    let result = exec(
        &rt,
        "SELECT name, queue_mode FROM red.collections WHERE name = 'users'",
    );

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(result.result.records[0].get("queue_mode"), Some(&Value::Null));
}
