use crate::application::entity::{
    PatchEntityInput, PatchEntityOperation, PatchEntityOperationType,
};
use crate::application::ports::RuntimeEntityPort;
use crate::{RedDBOptions, RedDBRuntime};

fn varied_payload(bytes: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";
    let mut state = 0x9e37_79b9u32;
    (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            ALPHABET[(state & 63) as usize] as char
        })
        .collect()
}

fn seeded_runtime(budget: u64, document: bool) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(budget))
        .expect("runtime");
    if document {
        runtime
            .execute_query("CREATE DOCUMENT growth")
            .expect("collection");
        runtime
            .execute_query("INSERT INTO growth DOCUMENT VALUES ({\"payload\":\"small\"})")
            .expect("seed");
    } else {
        runtime
            .execute_query("CREATE TABLE growth (payload TEXT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO growth (payload) VALUES ('small')")
            .expect("seed");
    }
    runtime
}

#[test]
fn mutation_memory_oversized_update_preserves_table_and_document() {
    for document in [false, true] {
        let runtime = seeded_runtime(128 * 1024, document);
        let before = runtime
            .execute_query("SELECT * FROM growth")
            .expect("before")
            .result
            .records;
        let result = runtime.execute_query(&format!(
            "UPDATE growth SET payload = '{}'",
            varied_payload(512 * 1024)
        ));
        assert!(
            result.is_err(),
            "growth must be denied: document={document}"
        );
        assert!(result
            .expect_err("denied")
            .to_string()
            .contains("over budget"));
        let after = runtime
            .execute_query("SELECT * FROM growth")
            .expect("after")
            .result
            .records;
        assert_eq!(
            format!("{before:?}"),
            format!("{after:?}"),
            "denial cannot publish an update"
        );
    }
}

#[test]
fn mutation_memory_successful_update_accounts_for_payload_and_old_version() {
    for document in [false, true] {
        let runtime = seeded_runtime(4 * 1024 * 1024, document);
        runtime.refresh_memory_accounting();
        let before = runtime.memory_accounting().total_used_bytes();
        let bytes = 64 * 1024;
        let payload = varied_payload(bytes);
        runtime
            .execute_query(&format!("UPDATE growth SET payload = '{}'", payload))
            .expect("within budget");
        runtime.refresh_memory_accounting();
        let after = runtime.memory_accounting().total_used_bytes();
        let expected_minimum = if document { bytes / 2 } else { bytes } as u64;
        assert_eq!(
            runtime
                .execute_query("SELECT payload FROM growth")
                .expect("readback")
                .result
                .records[0]
                .get("payload"),
            Some(&reddb_types::Value::text(payload))
        );
        assert!(
            after >= before + expected_minimum,
            "new payload must be charged: before={before}, after={after}, document={document}"
        );
    }
}

#[test]
fn mutation_memory_native_patch_rejects_before_publication() {
    let runtime = seeded_runtime(128 * 1024, true);
    let manager = runtime
        .db()
        .store()
        .get_collection("growth")
        .expect("collection");
    let before = manager.query_all(|_| true).remove(0);
    let result = runtime.patch_entity(PatchEntityInput {
        collection: "growth".to_string(),
        id: before.id,
        payload: crate::json::Value::Null,
        operations: vec![PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["body".to_string(), "payload".to_string()],
            value: Some(crate::json::json!(varied_payload(512 * 1024))),
        }],
    });
    assert!(result
        .expect_err("native PATCH must enforce the same budget")
        .to_string()
        .contains("over budget"));
    assert_eq!(
        format!("{:?}", before.data),
        format!("{:?}", manager.get(before.id).expect("unchanged").data)
    );
}

fn patch_payload(
    runtime: &RedDBRuntime,
    id: crate::storage::EntityId,
    payload: &str,
) -> crate::RedDBResult<()> {
    runtime
        .patch_entity(PatchEntityInput {
            collection: "growth".to_string(),
            id,
            payload: crate::json::Value::Null,
            operations: vec![PatchEntityOperation {
                op: PatchEntityOperationType::Set,
                path: vec!["fields".to_string(), "payload".to_string()],
                value: Some(crate::json::json!(payload)),
            }],
        })
        .map(|_| ())
}

#[test]
fn mutation_memory_native_patch_updates_growing_and_sealed_counters() {
    for sealed in [false, true] {
        let runtime = seeded_runtime(4 * 1024 * 1024, false);
        let manager = runtime
            .db()
            .store()
            .get_collection("growth")
            .expect("collection");
        let id = manager.query_all(|_| true)[0].id;
        if sealed {
            manager.force_seal().expect("seal");
        }
        let before = manager.resident_bytes();
        patch_payload(&runtime, id, &"x".repeat(64 * 1024)).expect("grow");
        let grown = manager.resident_bytes();
        assert!(
            grown >= before + 64 * 1024,
            "sealed={sealed}: {before} -> {grown}"
        );
        patch_payload(&runtime, id, "small").expect("shrink");
        assert!(
            manager.resident_bytes() < grown,
            "in-place payload was released"
        );
        assert!(
            manager.resident_bytes() > before,
            "zone bounds still retain the larger payload"
        );
    }
}

#[test]
fn mutation_memory_late_budget_denial_does_not_publish_earlier_chunks() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE growth (id INT, payload TEXT)")
        .expect("table");
    for start in (0..3000).step_by(500) {
        let rows = (start..start + 500)
            .map(|id| format!("({id}, 'small')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!("INSERT INTO growth (id,payload) VALUES {rows}"))
            .expect("seed");
    }
    runtime.refresh_memory_accounting();
    let fields = vec![
        ("id".to_string(), reddb_types::Value::Integer(0)),
        (
            "payload".to_string(),
            reddb_types::Value::text("x".repeat(3000)),
        ),
    ];
    let per_row = super::memory_admission::estimate_row_growth(&fields)
        + runtime.index_store_ref().estimate_insert_growth(
            "growth",
            std::iter::once(fields.as_slice()),
            false,
        );
    let headroom =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    let _held = runtime
        .admit_non_evictable_growth(
            crate::storage::memory_pools::MemoryPool::SegmentArena,
            "other work",
            headroom - per_row * 2100,
        )
        .expect("leave space beyond one chunk, but not all rows");
    let result = runtime.execute_query(&format!(
        "UPDATE growth SET payload = '{}'",
        "x".repeat(3000)
    ));
    assert!(
        result.is_err(),
        "all replacement versions exceed the budget"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM growth WHERE payload = 'small'")
            .expect("readback")
            .result
            .records
            .len(),
        3000,
        "no earlier chunk can survive a later admission denial"
    );
}

#[test]
fn mutation_memory_index_growth_is_reserved_before_data_changes() {
    use crate::storage::memory_pools::MemoryPool;
    let runtime = seeded_runtime(256 * 1024, false);
    runtime
        .execute_query("CREATE INDEX payload_index ON growth (payload) USING BTREE")
        .expect("index");
    runtime.refresh_memory_accounting();
    let headroom =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    // Enough for the row, not both sorted and equality keys of the BTree.
    let held = runtime
        .admit_non_evictable_growth(
            MemoryPool::SegmentArena,
            "competing writer",
            headroom - 90 * 1024,
        )
        .expect("reserve");
    let sql = format!("UPDATE growth SET payload = '{}'", "x".repeat(16 * 1024));
    assert!(
        runtime.execute_query(&sql).is_err(),
        "index bytes must participate in admission"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT payload FROM growth WHERE payload = 'small'")
            .expect("old index read")
            .result
            .records
            .len(),
        1
    );
    drop(held);
    runtime
        .execute_query(&sql)
        .expect("retry after reservation returns");
    assert_eq!(
        runtime
            .execute_query("SELECT payload FROM growth WHERE payload = 'small'")
            .expect("old index entry removed")
            .result
            .records
            .len(),
        0
    );
}

#[test]
fn mutation_memory_reservation_survives_publication_until_indexes_finish() {
    use super::index_store::MutationTestPhase;
    use crate::storage::memory_pools::MemoryPool;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    let runtime = seeded_runtime(256 * 1024, false);
    let (published_send, published_receive) = mpsc::channel();
    let (resume_send, resume_receive) = mpsc::channel();
    let resume_receive = parking_lot::Mutex::new(resume_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == "growth" && phase == MutationTestPhase::StoragePublished {
            published_send.send(()).expect("notify publication");
            resume_receive
                .lock()
                .recv_timeout(Duration::from_secs(15))
                .expect("resume writer");
        }
    }));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            runtime.execute_query(&format!(
                "UPDATE growth SET payload = '{}'",
                "x".repeat(32 * 1024)
            ))
        });
        published_receive
            .recv_timeout(Duration::from_secs(15))
            .expect("storage publication");
        runtime.refresh_memory_accounting();
        let reserved = runtime.inner.memory_reservations.lock().reserved_bytes;
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let rejected = runtime
            .admit_non_evictable_growth(
                MemoryPool::IndexMemory,
                "concurrent index writer",
                headroom,
            )
            .is_err();
        resume_send.send(()).expect("resume writer");
        writer
            .join()
            .expect("writer thread")
            .expect("writer succeeds");
        assert!(
            reserved >= 32 * 1024,
            "reservation must outlive storage publication"
        );
        assert!(
            rejected,
            "concurrent writer cannot spend in-flight index headroom"
        );
    });
    *runtime.index_store_ref().mutation_hook.lock() = None;
    runtime.refresh_memory_accounting();
    assert_eq!(runtime.inner.memory_reservations.lock().reserved_bytes, 0);
}

#[test]
fn mutation_memory_wal_child() {
    let Some(path) = std::env::var_os("REDDB_MUTATION_MEMORY_WAL_TEST_PATH") else {
        return;
    };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(path)
            .with_memory_budget(4 * 1024 * 1024)
            .with_auto_checkpoint(0),
    )
    .expect("persistent runtime");
    runtime
        .execute_query("CREATE TABLE growth (payload TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO growth (payload) VALUES ('small')")
        .expect("seed");
    runtime
        .execute_query("UPDATE growth SET payload = 'committed'")
        .expect("durable update");
    assert!(runtime
        .execute_query(&format!(
            "UPDATE growth SET payload = '{}'",
            "x".repeat(8 * 1024 * 1024)
        ))
        .is_err());
    // Simulate process loss without running runtime/storage destructors.
    std::process::exit(0);
}

#[test]
fn mutation_memory_wal_recovery_preserves_success_and_excludes_denied_update() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("growth.rdb");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "runtime::mutation_memory_tests::mutation_memory_wal_child",
            "--nocapture",
        ])
        .env("REDDB_MUTATION_MEMORY_WAL_TEST_PATH", &path)
        .output()
        .expect("child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path).with_memory_budget(4 * 1024 * 1024),
    )
    .expect("reopen");
    assert_eq!(
        runtime
            .execute_query("SELECT payload FROM growth WHERE payload = 'committed'")
            .expect("recovered")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn mutation_memory_late_constraint_error_finishes_published_indexes() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE growth (id INT, record_key INT UNIQUE)")
        .expect("table");
    runtime
        .execute_query("CREATE INDEX growth_lookup ON growth (record_key) USING BTREE")
        .expect("index");
    for start in (0..3000).step_by(500) {
        let rows = (start..start + 500)
            .map(|id| format!("({id}, {id})"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!("INSERT INTO growth (id,record_key) VALUES {rows}"))
            .expect("seed");
    }
    // The first chunk has distinct new keys; the last target collides with
    // its first published key. Preparing everything must preserve this check.
    let error = runtime
        .execute_query(
            "UPDATE growth SET record_key = CASE WHEN id = 2999 THEN 10000 ELSE id + 10000 END",
        )
        .expect_err("cross-chunk collision");
    assert!(
        error.to_string().to_lowercase().contains("unique"),
        "{error}"
    );
    let rows = runtime
        .execute_query("SELECT id FROM growth WHERE record_key = 10000")
        .expect("published index remains readable")
        .result
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&reddb_types::Value::Integer(0)));
    assert!(runtime
        .execute_query("SELECT id FROM growth WHERE record_key = 0")
        .expect("old version hidden")
        .result
        .records
        .is_empty());
}
