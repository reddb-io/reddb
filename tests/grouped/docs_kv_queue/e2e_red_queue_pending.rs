//! Issue #536 — QueueLifecycle slice 9: `red.queue_pending` virtual
//! table integration coverage. Drives the user-facing
//! enqueue → deliver path and asserts the per-row pending surface
//! shows up with the contracted columns.

use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_auth_identity, set_current_connection_id, set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

const QUEUE_PENDING_COLUMNS: [&str; 8] = [
    "queue",
    "group",
    "message_id",
    "delivery_id",
    "key",
    "attempts",
    "lock_deadline",
    "locked_by",
];

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

#[test]
fn red_queue_pending_exposes_ordering_key() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE QUEUE keyed WITH MAX_ATTEMPTS 3");
    exec(&rt, "QUEUE GROUP CREATE keyed workers");
    exec(&rt, "QUEUE PUSH keyed 'job' KEY 'tenant-7'");
    exec(
        &rt,
        "QUEUE READ keyed GROUP workers CONSUMER worker1 COUNT 1",
    );

    let result = rt
        .execute_query("SELECT queue, key FROM red.queue_pending")
        .expect("red.queue_pending select")
        .result;

    assert_eq!(result.records.len(), 1);
    let row = &result.records[0];
    assert_eq!(row.get("queue"), Some(&Value::text("keyed")));
    assert_eq!(row.get("key"), Some(&Value::text("tenant-7")));

    cleanup_scope();
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn cleanup_scope() {
    clear_current_auth_identity();
    clear_current_tenant();
    clear_current_connection_id();
}

#[test]
fn red_queue_pending_lists_active_delivery_row() {
    cleanup_scope();
    let rt = runtime();
    exec(
        &rt,
        "CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3",
    );
    exec(&rt, "QUEUE GROUP CREATE tasks workers");
    exec(&rt, "QUEUE PUSH tasks 'job-1'");
    exec(
        &rt,
        "QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1",
    );

    let result = rt
        .execute_query("SELECT * FROM red.queue_pending")
        .expect("red.queue_pending select")
        .result;

    // Schema shape — slice-9 contract.
    let cols: Vec<&str> = result.columns.iter().map(String::as_str).collect();
    assert_eq!(cols, QUEUE_PENDING_COLUMNS);

    assert_eq!(result.records.len(), 1, "one pending row after deliver");
    let row = &result.records[0];
    assert_eq!(row.get("queue"), Some(&Value::text("tasks")));
    assert_eq!(row.get("group"), Some(&Value::text("workers")));
    assert_eq!(row.get("locked_by"), Some(&Value::text("worker1")));
    // First delivery: attempts == 0.
    assert!(matches!(
        row.get("attempts"),
        Some(Value::UnsignedInteger(0))
    ));
    // delivery_id is an opaque server-issued handle; assert only that
    // the pending row exposes a non-empty handle.
    match row.get("delivery_id") {
        Some(Value::Text(value)) => {
            assert!(!value.is_empty());
        }
        other => panic!("expected delivery_id text, got {other:?}"),
    }
    // lock_deadline is a derived timestamp; just assert it's a
    // TimestampMs and strictly positive (a 30s default deadline
    // anchored to now() is always > 0).
    match row.get("lock_deadline") {
        Some(Value::TimestampMs(value)) => assert!(*value > 0),
        other => panic!("expected lock_deadline TimestampMs, got {other:?}"),
    }

    cleanup_scope();
}

#[test]
fn red_queue_pending_is_empty_before_deliver() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE QUEUE q WITH MAX_ATTEMPTS 3");
    exec(&rt, "QUEUE GROUP CREATE q g");
    exec(&rt, "QUEUE PUSH q 'msg'");

    let result = rt
        .execute_query("SELECT * FROM red.queue_pending")
        .expect("red.queue_pending select")
        .result;
    assert_eq!(result.records.len(), 0);

    cleanup_scope();
}

#[test]
fn red_queue_pending_tenant_filtering() {
    cleanup_scope();
    let rt = runtime();
    exec(&rt, "CREATE QUEUE acme_jobs WITH MAX_ATTEMPTS 3");
    exec(&rt, "QUEUE GROUP CREATE acme_jobs workers");
    exec(&rt, "QUEUE PUSH acme_jobs 'job'");
    exec(
        &rt,
        "QUEUE READ acme_jobs GROUP workers CONSUMER worker1 COUNT 1",
    );

    // Non-admin without a tenant is rejected (ADR-0011 read access).
    set_current_connection_id(53601);
    set_current_auth_identity("alice".to_string(), Role::Read);
    let err = rt
        .execute_query("SELECT * FROM red.queue_pending")
        .expect_err("tenant-less non-admin should be rejected")
        .to_string();
    assert!(err.contains("active tenant"), "error was: {err}");

    // Tenant-scoped read returns rows for collections visible in
    // the active scope. With no explicit per-collection tenant
    // binding, `acme_jobs` is in scope for any active tenant — the
    // important assertion here is that the scope path is exercised
    // (no panic, well-formed result), not the cross-tenant negative
    // which is owned by the shared `collection_is_visible` helper
    // already tested in `e2e_red_schema`.
    set_current_tenant("acme".to_string());
    let result = rt
        .execute_query("SELECT * FROM red.queue_pending")
        .expect("scoped pending select")
        .result;
    assert_eq!(result.records.len(), 1);

    // Admin bypass — sees the row.
    cleanup_scope();
    set_current_connection_id(53602);
    set_current_auth_identity("root".to_string(), Role::Admin);
    let result = rt
        .execute_query("SELECT * FROM red.queue_pending")
        .expect("admin pending select")
        .result;
    assert_eq!(result.records.len(), 1);

    cleanup_scope();
}

#[test]
fn red_queue_pending_is_read_only() {
    cleanup_scope();
    let rt = runtime();
    for sql in [
        "INSERT INTO red.queue_pending (queue) VALUES ('x')",
        "DELETE FROM red.queue_pending WHERE queue = 'x'",
    ] {
        let err = match rt.execute_query(sql) {
            Ok(_) => panic!("expected read-only error for {sql}"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("system schema is read-only"),
            "{sql} returned unexpected error: {err}"
        );
    }
    cleanup_scope();
}
