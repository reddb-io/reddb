//! Physical relocation must not change an uncommitted update's durable preimage.
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::storage::EntityId;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn growing_preimage_keeps_identity_through_pending_checkpoint_and_rollback() {
    qualify_preimage(false);
}

#[test]
fn sealed_preimage_keeps_identity_through_pending_checkpoint_and_rollback() {
    qualify_preimage(true);
}

fn seed(runtime: &RedDBRuntime) {
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled=false",
        "CREATE TABLE checkpoint_payload (id INT,n INT,payload TEXT)",
        "CREATE UNIQUE INDEX checkpoint_payload_id ON checkpoint_payload (id)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    let payload = "p".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id},0,'{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO checkpoint_payload (id,n,payload) VALUES {rows}"
            ))
            .expect("seed");
    }
}

fn identity(runtime: &RedDBRuntime, logical_id: u64) -> (EntityId, u64) {
    let manager = runtime
        .db()
        .store()
        .get_collection("checkpoint_payload")
        .expect("collection");
    let mut result = None;
    manager.for_each_entity(|entity| {
        if entity.data.as_row().expect("row").columns[0].to_string() == logical_id.to_string() {
            result = Some((entity.id, entity.sequence_id));
            return false;
        }
        true
    });
    result.expect("row identity")
}

fn qualify_preimage(seal_before_update: bool) {
    clear_current_connection_id();
    let directory = tempfile::tempdir().expect("temporary native store");
    let path = directory.path().join("preimage.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    seed(&runtime);
    runtime.checkpoint().expect("baseline checkpoint");
    drop(runtime);
    let runtime = RedDBRuntime::with_options(options()).expect("cold reopen");
    let manager = runtime
        .db()
        .store()
        .get_collection("checkpoint_payload")
        .expect("collection");
    let (preimage_id, sequence_before) = identity(&runtime, 999);
    assert!(sequence_before > 0);
    if seal_before_update {
        manager.force_seal().expect("seal preimage");
    }
    set_current_connection_id(101);
    runtime.execute_query("BEGIN").expect("pending writer");
    runtime
        .execute_query("UPDATE checkpoint_payload SET n=9999 WHERE id=999")
        .expect("pending update");
    set_current_connection_id(202);
    runtime
        .execute_query("UPDATE checkpoint_payload SET n=77 WHERE id=998")
        .expect("committed neighbour");
    let sequence_during_pending = manager
        .get(preimage_id)
        .expect("pending preimage")
        .sequence_id;
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    runtime
        .checkpoint()
        .expect("checkpoint with pending preimage");
    let after = pager.cache_stats();
    let pending_reads = after.hits + after.misses - before.hits - before.misses;
    set_current_connection_id(101);
    runtime.execute_query("ROLLBACK").expect("rollback");
    set_current_connection_id(202);
    let before = pager.cache_stats();
    runtime.checkpoint().expect("checkpoint after rollback");
    let after = pager.cache_stats();
    let rollback_reads = after.hits + after.misses - before.hits - before.misses;
    let sequence_after_rollback = manager
        .get(preimage_id)
        .expect("restored preimage")
        .sequence_id;
    runtime
        .execute_query("INSERT INTO checkpoint_payload (id,n,payload) VALUES (1001,1,'next')")
        .expect("insert after relocation");
    let next_sequence = identity(&runtime, 1001).1;
    eprintln!("sealed={seal_before_update}, sequence={sequence_before}/{sequence_during_pending}/{sequence_after_rollback}, next={next_sequence}, checkpoint reads={pending_reads}/{rollback_reads}");
    drop(pager);
    drop(manager);
    drop(runtime);
    clear_current_connection_id();
    let recovered = RedDBRuntime::with_options(options()).expect("reopen committed projection");
    for (id, n) in [(999, 0), (998, 77), (1001, 1)] {
        let read = recovered
            .execute_query(&format!("SELECT n FROM checkpoint_payload WHERE id={id}"))
            .expect("durable readback");
        assert_eq!(
            read.result.records[0].get("n").expect("n").to_string(),
            n.to_string()
        );
    }
    let plan = recovered
        .execute_query("EXPLAIN SELECT n FROM checkpoint_payload WHERE id=999")
        .expect("recovered index");
    assert!(plan.result.records.iter().any(|row| row
        .get("op")
        .is_some_and(|value| value == &reddb_types::Value::text("index_seek"))));
    assert_eq!(sequence_during_pending, sequence_before);
    assert_eq!(sequence_after_rollback, sequence_before);
    assert!(
        next_sequence > sequence_before,
        "allocator reused an adopted sequence"
    );
    assert!(
        pending_reads < 128 && rollback_reads < 128,
        "preimage relocation scanned history: pending={pending_reads}, rollback={rollback_reads}"
    );
}
