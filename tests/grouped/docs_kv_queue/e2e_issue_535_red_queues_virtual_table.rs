//! Regression coverage for issue #535 — QueueLifecycle slice 8.
//!
//! Pins the `red.queues` virtual table contract and the `SHOW QUEUES`
//! desugar repoint:
//!
//! 1. `red.queues` exposes queue-shaped columns
//!    (`name, mode, depth, total_pending, oldest_pending_age,
//!    dlq_target, attention, internal`) — no queue-irrelevant
//!    `red.collections` columns leaking through.
//! 2. `SHOW QUEUES` reads from `red.queues` and hides DLQ-target
//!    queues by default; `SHOW QUEUES INCLUDING INTERNAL` surfaces
//!    them.
//! 3. `total_pending` rises when a message is delivered (read) and
//!    `oldest_pending_age` becomes non-NULL — derived from
//!    `red_queue_meta`, not the catalog descriptor's hot fields.

#[path = "../../support/mod.rs"]
mod support;

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn red_queues_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn unique_ident(prefix: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{unique}")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn rows(result: &RuntimeQueryResult) -> &[UnifiedRecord] {
    &result.result.records
}

fn find_row<'a>(result: &'a RuntimeQueryResult, name: &str) -> Option<&'a UnifiedRecord> {
    rows(result).iter().find(|row| match row.get("name") {
        Some(Value::Text(value)) => value.as_ref() == name,
        _ => false,
    })
}

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column `{column}`, got {other:?}"),
    }
}

fn uint(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned column `{column}`, got {other:?}"),
    }
}

fn message_id_of(result: &RuntimeQueryResult) -> String {
    match result.result.records[0].get("message_id") {
        Some(Value::Text(value)) => value.to_string(),
        Some(Value::UnsignedInteger(value)) => value.to_string(),
        other => panic!("unexpected message_id: {other:?}"),
    }
}

#[test]
fn red_queues_exposes_queue_shaped_columns() {
    let _guard = red_queues_test_lock().lock().unwrap();
    let rt = runtime();
    let jobs_name = unique_ident("jobs");
    let broadcasts_name = unique_ident("broadcasts");
    let tasks_name = unique_ident("tasks");
    let failed_tasks_name = unique_ident("failed_tasks");

    exec(&rt, &format!("CREATE QUEUE {jobs_name}"));
    exec(&rt, &format!("CREATE QUEUE {broadcasts_name} FANOUT"));
    exec(
        &rt,
        &format!("CREATE QUEUE {tasks_name} WITH DLQ {failed_tasks_name} MAX_ATTEMPTS 2"),
    );

    let result = exec(&rt, "SELECT * FROM red.queues");

    // The faithful-to-type rule from ADR-0020 / glossary:
    // queue users only see queue-shaped columns. No `schema_mode`,
    // `entities`, `vector_dimension`, etc. — those belong on
    // `red.collections`.
    let expected: std::collections::HashSet<&str> = [
        "name",
        "mode",
        "depth",
        "total_pending",
        "oldest_pending_age",
        "dlq_target",
        "attention",
        "internal",
    ]
    .into_iter()
    .collect();
    let actual: std::collections::HashSet<&str> =
        result.result.columns.iter().map(String::as_str).collect();
    assert_eq!(
        actual, expected,
        "red.queues column set must match the slice 8 contract"
    );

    // Work-mode queue surfaces WORK and the absent DLQ target as
    // NULL — `name` matching keeps the assertion stable across
    // any test-ordering surprises.
    let jobs = find_row(&result, &jobs_name).expect("jobs row should be present");
    assert_eq!(text(jobs, "mode"), "WORK");
    assert!(matches!(jobs.get("dlq_target"), Some(Value::Null)));

    // Fanout-mode queue surfaces the upper-case mode literal so the
    // user-facing `SHOW QUEUES` output is faithful to the
    // `(WORK|FANOUT)` brief.
    let broadcasts = find_row(&result, &broadcasts_name).expect("broadcasts row");
    assert_eq!(text(broadcasts, "mode"), "FANOUT");

    // `WITH DLQ failed_tasks` populates the dlq_target hot field.
    let tasks = find_row(&result, &tasks_name).expect("tasks row");
    assert_eq!(text(tasks, "dlq_target"), failed_tasks_name);
}

#[test]
fn show_queues_desugar_repointed_and_hides_internal_dlq() {
    let _guard = red_queues_test_lock().lock().unwrap();
    let rt = runtime();
    let tasks_name = unique_ident("tasks");
    let failed_tasks_name = unique_ident("failed_tasks");

    exec(
        &rt,
        &format!("CREATE QUEUE {tasks_name} WITH DLQ {failed_tasks_name} MAX_ATTEMPTS 2"),
    );

    // SHOW QUEUES → red.queues, internal DLQ-target queues hidden
    // by default.
    let visible = exec(&rt, "SHOW QUEUES");
    assert!(
        find_row(&visible, &tasks_name).is_some(),
        "user-declared queue must be visible: {:?}",
        rows(&visible)
    );
    assert!(
        find_row(&visible, &failed_tasks_name).is_none(),
        "DLQ-target queue must be hidden by default: {:?}",
        rows(&visible)
    );

    // INCLUDING INTERNAL surfaces the DLQ target.
    let with_internal = exec(&rt, "SHOW QUEUES INCLUDING INTERNAL");
    assert!(
        find_row(&with_internal, &tasks_name).is_some(),
        "user queue must still be present"
    );
    assert!(
        find_row(&with_internal, &failed_tasks_name).is_some(),
        "INCLUDING INTERNAL must surface the DLQ target: {:?}",
        rows(&with_internal)
    );
}

#[test]
fn total_pending_and_oldest_age_reflect_live_meta_state() {
    let _guard = red_queues_test_lock().lock().unwrap();
    let rt = runtime();
    let jobs_name = unique_ident("jobs");
    let workers_name = unique_ident("workers");
    let worker_name = unique_ident("worker");

    exec(&rt, &format!("CREATE QUEUE {jobs_name}"));
    exec(
        &rt,
        &format!("QUEUE GROUP CREATE {jobs_name} {workers_name}"),
    );

    // Empty queue: total_pending = 0, oldest_pending_age NULL.
    let initial = exec(
        &rt,
        &format!("SELECT * FROM red.queues WHERE name = '{jobs_name}'"),
    );
    let row = find_row(&initial, &jobs_name).expect("jobs row");
    assert_eq!(uint(row, "total_pending"), 0);
    assert!(matches!(row.get("oldest_pending_age"), Some(Value::Null)));

    // Push + read marks a delivery pending in `red_queue_meta`,
    // which the snapshot scans.
    exec(&rt, &format!("QUEUE PUSH {jobs_name} 'first-job'"));
    let read = exec(
        &rt,
        &format!("QUEUE READ {jobs_name} GROUP {workers_name} CONSUMER {worker_name} COUNT 1"),
    );
    assert_eq!(
        read.result.records.len(),
        1,
        "read should deliver one message"
    );

    let after = exec(
        &rt,
        &format!("SELECT * FROM red.queues WHERE name = '{jobs_name}'"),
    );
    let row = find_row(&after, &jobs_name).expect("jobs row after read");
    assert_eq!(
        uint(row, "total_pending"),
        1,
        "one delivery should be pending after READ"
    );
    assert!(
        matches!(
            row.get("oldest_pending_age"),
            Some(Value::UnsignedInteger(_))
        ),
        "oldest_pending_age must be populated when pending > 0, got {:?}",
        row.get("oldest_pending_age")
    );
}

#[test]
fn show_queues_response_uses_red_queues_column_set() {
    let _guard = red_queues_test_lock().lock().unwrap();
    // Belt-and-braces: pin that `SHOW QUEUES` returns the
    // queue-shaped column set, not the `red.collections` projection.
    let rt = runtime();
    let jobs_name = unique_ident("jobs");
    exec(&rt, &format!("CREATE QUEUE {jobs_name}"));

    let result = exec(&rt, "SHOW QUEUES");
    let cols: std::collections::HashSet<&str> =
        result.result.columns.iter().map(String::as_str).collect();

    // Sentinel: presence of queue-only columns + absence of a
    // characteristic red.collections-only column.
    assert!(cols.contains("mode"), "must include `mode`");
    assert!(cols.contains("depth"), "must include `depth`");
    assert!(cols.contains("dlq_target"), "must include `dlq_target`");
    assert!(
        !cols.contains("schema_mode"),
        "`schema_mode` is a red.collections column and must not leak"
    );
    assert!(
        !cols.contains("vector_dimension"),
        "`vector_dimension` is a red.collections column and must not leak"
    );

    // Read a row through to exercise the snapshot path (vs only the
    // declared schema).
    let jobs = find_row(&result, &jobs_name).expect("jobs row in SHOW QUEUES");
    assert_eq!(text(jobs, "mode"), "WORK");
    // Drop unused warning suppression — message_id_of is reserved
    // for follow-up scenarios.
    let _ = message_id_of;
}
