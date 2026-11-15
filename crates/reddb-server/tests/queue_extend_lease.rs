//! Queue worker lease renewal through an opaque `delivery_id`.

use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn delivery_id(result: &reddb_server::RuntimeQueryResult) -> String {
    match result.result.records[0].get("delivery_id") {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text delivery_id, got {other:?}"),
    }
}

fn lock_deadline_ns(runtime: &RedDBRuntime, delivery_id: &str) -> u64 {
    let result = runtime
        .execute_query("SELECT * FROM red_queue_meta")
        .expect("queue metadata is queryable");
    result
        .result
        .records
        .iter()
        .find_map(|record| {
            let matches = matches!(
                record.get("delivery_id"),
                Some(Value::Text(value)) if value.as_ref() == delivery_id
            );
            if !matches {
                return None;
            }
            match record.get("lock_deadline_ns") {
                Some(Value::UnsignedInteger(value)) => Some(*value),
                Some(Value::Integer(value)) => u64::try_from(*value).ok(),
                _ => None,
            }
        })
        .expect("pending delivery has a lock deadline")
}

#[test]
fn queue_extend_uses_current_queue_lock_policy() {
    let runtime = runtime();
    runtime
        .execute_query("CREATE QUEUE jobs LOCK_DEADLINE_MS 1000")
        .expect("create queue");
    runtime
        .execute_query("QUEUE PUSH jobs 'job-1'")
        .expect("push");
    let read = runtime
        .execute_query("QUEUE READ jobs CONSUMER worker COUNT 1")
        .expect("read");
    let delivery_id = delivery_id(&read);
    let deadline_before = lock_deadline_ns(&runtime, &delivery_id);

    runtime
        .execute_query("ALTER QUEUE jobs SET LOCK_DEADLINE_MS 60000")
        .expect("alter queue policy");
    runtime
        .execute_query(&format!(
            "QUEUE EXTEND jobs WITH delivery_id = '{delivery_id}'"
        ))
        .expect("extend delivery lease");
    let deadline_after = lock_deadline_ns(&runtime, &delivery_id);

    assert!(
        deadline_after > deadline_before.saturating_add(30_000_000_000),
        "extend must refresh from the queue's current lock policy"
    );
}

#[test]
fn queue_extend_rejects_a_delivery_from_another_queue() {
    let runtime = runtime();
    runtime.execute_query("CREATE QUEUE jobs").expect("jobs");
    runtime.execute_query("CREATE QUEUE other").expect("other");
    runtime
        .execute_query("QUEUE PUSH jobs 'job-1'")
        .expect("push");
    let read = runtime
        .execute_query("QUEUE READ jobs CONSUMER worker COUNT 1")
        .expect("read");
    let delivery_id = delivery_id(&read);

    let error = runtime
        .execute_query(&format!(
            "QUEUE EXTEND other WITH delivery_id = '{delivery_id}'"
        ))
        .expect_err("cross-queue lease extension must fail");
    assert!(
        error
            .to_string()
            .contains("does not belong to queue 'other'"),
        "unexpected error: {error}"
    );
}

#[test]
fn queue_extend_rejects_a_delivery_after_ack() {
    let runtime = runtime();
    runtime.execute_query("CREATE QUEUE jobs").expect("jobs");
    runtime
        .execute_query("QUEUE PUSH jobs 'job-1'")
        .expect("push");
    let read = runtime
        .execute_query("QUEUE READ jobs CONSUMER worker COUNT 1")
        .expect("read");
    let delivery_id = delivery_id(&read);

    runtime
        .execute_query(&format!(
            "QUEUE ACK jobs WITH delivery_id = '{delivery_id}'"
        ))
        .expect("ack");
    let error = runtime
        .execute_query(&format!(
            "QUEUE EXTEND jobs WITH delivery_id = '{delivery_id}'"
        ))
        .expect_err("an acknowledged delivery cannot be extended");

    assert!(
        error.to_string().contains("live pending delivery"),
        "unexpected error: {error}"
    );
}
