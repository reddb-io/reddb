//! DDL round-trip coverage for the queue policy hot-fields tier
//! introduced in PRD #527 slice 6 (#530).
//!
//! `CREATE QUEUE` with all policy clauses persists to the catalog
//! snapshot's `CollectionDescriptor` hot-fields tier so `QueueLifecycle`
//! (future slices) can read them sub-ms. `ALTER QUEUE SET <clause>`
//! mutates them. Defaults apply when clauses are omitted.

use reddb_server::storage::query::{
    DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP, DEFAULT_QUEUE_LOCK_DEADLINE_MS,
    DEFAULT_QUEUE_MAX_ATTEMPTS,
};
use reddb_server::{RedDBOptions, RedDBRuntime};

fn descriptor_for(runtime: &RedDBRuntime, queue: &str) -> reddb_server::CollectionDescriptor {
    runtime
        .db()
        .catalog_model_snapshot()
        .collections
        .into_iter()
        .find(|c| c.name == queue)
        .expect("queue descriptor present in snapshot")
}

#[test]
fn create_queue_defaults_land_on_descriptor_hot_fields() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE q_defaults").expect("create");

    let desc = descriptor_for(&rt, "q_defaults");
    assert_eq!(desc.queue_max_attempts, Some(DEFAULT_QUEUE_MAX_ATTEMPTS));
    assert_eq!(
        desc.queue_lock_deadline_ms,
        Some(DEFAULT_QUEUE_LOCK_DEADLINE_MS)
    );
    assert_eq!(
        desc.queue_in_flight_cap_per_group,
        Some(DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP)
    );
    assert!(desc.queue_dlq_target.is_none(), "DLQ unset → on-max drop");
}

#[test]
fn create_queue_with_all_policy_clauses_round_trips_to_descriptor() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query(
        "CREATE QUEUE q_full \
         MAX_ATTEMPTS 9 \
         LOCK_DEADLINE_MS 45000 \
         IN_FLIGHT_CAP_PER_GROUP 250 \
         WITH DLQ q_full_dlq",
    )
    .expect("create with all policy clauses");

    let desc = descriptor_for(&rt, "q_full");
    assert_eq!(desc.queue_max_attempts, Some(9));
    assert_eq!(desc.queue_lock_deadline_ms, Some(45_000));
    assert_eq!(desc.queue_in_flight_cap_per_group, Some(250));
    assert_eq!(desc.queue_dlq_target.as_deref(), Some("q_full_dlq"));
}

#[test]
fn alter_queue_mutates_each_policy_field_independently() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE q_mut").expect("create");

    rt.execute_query("ALTER QUEUE q_mut SET MAX_ATTEMPTS 11")
        .expect("alter max_attempts");
    assert_eq!(descriptor_for(&rt, "q_mut").queue_max_attempts, Some(11));

    rt.execute_query("ALTER QUEUE q_mut SET LOCK_DEADLINE_MS 90000")
        .expect("alter lock_deadline_ms");
    assert_eq!(
        descriptor_for(&rt, "q_mut").queue_lock_deadline_ms,
        Some(90_000)
    );

    rt.execute_query("ALTER QUEUE q_mut SET IN_FLIGHT_CAP_PER_GROUP 17")
        .expect("alter in_flight_cap_per_group");
    assert_eq!(
        descriptor_for(&rt, "q_mut").queue_in_flight_cap_per_group,
        Some(17)
    );

    rt.execute_query("ALTER QUEUE q_mut SET DLQ q_mut_dlq")
        .expect("alter dlq");
    assert_eq!(
        descriptor_for(&rt, "q_mut").queue_dlq_target.as_deref(),
        Some("q_mut_dlq")
    );
}

#[test]
fn descriptor_omits_queue_policy_fields_for_non_queue_collections() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE TABLE t (id INT PRIMARY KEY)")
        .expect("create table");

    let desc = descriptor_for(&rt, "t");
    assert!(desc.queue_mode.is_none());
    assert!(desc.queue_max_attempts.is_none());
    assert!(desc.queue_lock_deadline_ms.is_none());
    assert!(desc.queue_in_flight_cap_per_group.is_none());
    assert!(desc.queue_dlq_target.is_none());
}

#[test]
fn alter_queue_rejects_dlq_equal_to_source() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE q_self").expect("create");
    let err = rt
        .execute_query("ALTER QUEUE q_self SET DLQ q_self")
        .expect_err("self-DLQ rejected");
    assert!(
        err.to_string().contains("dead-letter queue"),
        "{err}"
    );
}
