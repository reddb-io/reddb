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
    assert!(result
        .expect_err("all replacement versions exceed the budget")
        .to_string()
        .contains("over budget"));
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
    assert!(runtime
        .execute_query(&sql)
        .expect_err("index bytes must participate in admission")
        .to_string()
        .contains("over budget"));
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
    // Stay below the parser's 1 MiB input cap. The row plus retained
    // zone bounds exceed this runtime's 4 MiB memory budget.
    let denied = runtime
        .execute_query(&format!(
            "UPDATE growth SET payload = '{}'",
            "x".repeat(900 * 1024)
        ))
        .expect_err("memory admission must reject the replacement");
    assert!(denied.to_string().contains("over budget"), "{denied}");
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

#[test]
fn mutation_memory_native_vector_content_and_graph_type_are_governed() {
    use crate::application::entity::{CreateNodeInput, CreateVectorInput};
    for graph in [false, true] {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(4 * 1024 * 1024),
        )
        .expect("runtime");
        let (id, path) = if graph {
            runtime.execute_query("CREATE GRAPH growth").expect("graph");
            let id = runtime
                .create_node(CreateNodeInput {
                    collection: "growth".to_string(),
                    label: "seed".to_string(),
                    node_type: Some("small".to_string()),
                    properties: Vec::new(),
                    metadata: Vec::new(),
                    embeddings: Vec::new(),
                    table_links: Vec::new(),
                    node_links: Vec::new(),
                })
                .expect("node")
                .id;
            (id, vec!["node_type".to_string()])
        } else {
            let id = runtime
                .create_vector(CreateVectorInput {
                    collection: "growth".to_string(),
                    dense: vec![1.0, 0.0, 0.0],
                    content: Some("small".to_string()),
                    metadata: Vec::new(),
                    link_row: None,
                    link_node: None,
                })
                .expect("vector")
                .id;
            (id, vec!["fields".to_string(), "content".to_string()])
        };
        let manager = runtime
            .db()
            .store()
            .get_collection("growth")
            .expect("collection");
        let snapshot = manager.get(id).expect("before");
        let patch = |payload: String| {
            runtime.patch_entity(PatchEntityInput {
                collection: "growth".to_string(),
                id,
                payload: crate::json::Value::Null,
                operations: vec![PatchEntityOperation {
                    op: PatchEntityOperationType::Set,
                    path: path.clone(),
                    value: Some(crate::json::json!(payload)),
                }],
            })
        };
        let error =
            patch("x".repeat(8 * 1024 * 1024)).expect_err("variable payload exceeds budget");
        assert!(
            error.to_string().contains("over budget"),
            "graph={graph}: {error}"
        );
        let unchanged = manager.get(id).expect("unchanged");
        assert_eq!(unchanged.kind, snapshot.kind);
        assert_eq!(
            format!("{:?}", unchanged.data),
            format!("{:?}", snapshot.data)
        );
        let before = manager.resident_bytes();
        patch("x".repeat(64 * 1024)).expect("admitted payload");
        assert!(
            manager.resident_bytes() >= before + 64 * 1024 - 5,
            "graph={graph}: charge the admitted payload"
        );
    }
}

#[test]
fn mutation_memory_batch_amortizes_pool_sampling() {
    let runtime = seeded_runtime(4 * 1024 * 1024, false);
    for _ in 1..100 {
        runtime
            .execute_query("INSERT INTO growth (payload) VALUES ('small')")
            .expect("seed");
    }
    let before = runtime.inner.memory_reservations.lock().samples;
    runtime
        .execute_query("UPDATE growth SET payload = 'updated'")
        .expect("batch update");
    let samples = runtime.inner.memory_reservations.lock().samples - before;
    assert!(
        samples < 10,
        "100 small mutations must not sample every pool per row: {samples}"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT payload FROM growth WHERE payload = 'updated'")
            .expect("readback")
            .result
            .records
            .len(),
        100
    );
}

#[test]
fn mutation_memory_batch_optional_slack_cannot_deny_a_fitting_update() {
    let runtime = seeded_runtime(128 * 1024, false);
    runtime.refresh_memory_accounting();
    let headroom =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    let _held = runtime
        .admit_non_evictable_growth(
            crate::storage::memory_pools::MemoryPool::IndexMemory,
            "competing writer",
            headroom - 8 * 1024,
        )
        .expect("leave less than a reservation quantum");
    runtime
        .execute_query("UPDATE growth SET payload = 'updated'")
        .expect("the replacement fits without speculative slack");
    assert_eq!(
        runtime
            .execute_query("SELECT payload FROM growth WHERE payload = 'updated'")
            .expect("readback")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn bulk_update_wal_child() {
    let Some(path) = std::env::var_os("REDDB_BULK_UPDATE_WAL_TEST_PATH") else {
        return;
    };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_memory_budget(32 * 1024 * 1024)
            .with_auto_checkpoint(0),
    )
    .expect("persistent runtime");
    runtime
        .execute_query("CREATE TABLE batch_growth (id INT, payload TEXT)")
        .expect("table");
    for start in (0..2050).step_by(500) {
        let rows = (start..(start + 500).min(2050))
            .map(|id| format!("({id}, 'seed')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO batch_growth (id,payload) VALUES {rows}"
            ))
            .expect("seed");
    }
    let generation_before = crate::storage::EmbeddedRdbArtifact::open(&path)
        .expect("read initial durable boundary")
        .selected_superblock
        .generation;
    runtime
        .execute_query("UPDATE batch_growth SET payload = 'committed'")
        .expect("durable bulk update");
    let generation_after = crate::storage::EmbeddedRdbArtifact::open(&path)
        .expect("read committed durable boundary")
        .selected_superblock
        .generation;
    // Each embedded WAL append advances the durable superblock generation.
    // Two chunks require two appends; either chunk may additionally grow the
    // WAL region with a checkpoint. Per-row appends advanced it 2057 times.
    let publications = generation_after - generation_before;
    assert!(
        (2..=4).contains(&publications),
        "expected two chunk appends plus at most two growth checkpoints, got {publications}"
    );
    // Lose the process without a checkpoint or runtime/storage destructors.
    std::process::exit(0);
}

#[test]
fn bulk_update_wal_batches_and_recovers_every_key_after_process_exit() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("batch-growth.rdb");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "runtime::mutation_memory_tests::bulk_update_wal_child",
            "--nocapture",
        ])
        .env("REDDB_BULK_UPDATE_WAL_TEST_PATH", &path)
        .output()
        .expect("child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("reopen after process loss");
    let records = runtime
        .execute_query("SELECT id,payload FROM batch_growth ORDER BY id")
        .expect("recovered rows")
        .result
        .records;
    assert_eq!(records.len(), 2050);
    for (id, record) in records.iter().enumerate() {
        assert_eq!(
            record.get("id"),
            Some(&reddb_types::Value::Integer(
                i64::try_from(id).expect("small fixture")
            ))
        );
        assert_eq!(
            record.get("payload"),
            Some(&reddb_types::Value::text("committed")),
            "recovered payload for id={id}"
        );
    }
}

#[test]
fn recovered_named_rows_share_declared_schema_before_serving_queries() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("recovery-memory.rdb");
    let options = RedDBOptions::persistent(&path).with_memory_budget(32 * 1024 * 1024);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("create");
        runtime.execute_query("CREATE TABLE recovery_memory (id INTEGER PRIMARY KEY, payload TEXT, dimension INTEGER)").expect("table");
        for id in 0..128 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO recovery_memory (id,payload,dimension) VALUES ({id},'kept',{id})"
                ))
                .expect("insert");
        }
    }
    for _ in 0..3 {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("reopen");
        let manager = runtime
            .inner
            .db
            .store()
            .get_collection("recovery_memory")
            .expect("collection");
        let mut count = 0;
        manager.for_each_entity(|entity| {
            let row = entity.data.as_row().expect("row");
            assert!(
                row.named.is_none(),
                "recovery must compact the named hash map"
            );
            assert_eq!(
                row.get_field("payload"),
                Some(&reddb_types::Value::text("kept"))
            );
            count += 1;
            true
        });
        assert_eq!(count, 128);
        let result = runtime
            .execute_query(
                "SELECT id,dimension FROM recovery_memory WHERE dimension >= 120 ORDER BY id",
            )
            .expect("filtered rows");
        assert_eq!(result.result.records.len(), 8);
        for (index, row) in result.result.records.iter().enumerate() {
            assert_eq!(
                row.get("id"),
                Some(&reddb_types::Value::Integer(120 + index as i64))
            );
            assert_eq!(row.get("dimension"), row.get("id"));
        }
    }
}

#[test]
fn paged_recovery_compacts_named_rows_before_runtime_initialization() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("early-recovery-memory.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(crate::StorageDeployPreset::Serverless.selection())
        .expect("paged profile")
        .with_memory_budget(32 * 1024 * 1024);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("create");
        runtime
            .execute_query(
                "CREATE TABLE early_recovery_memory (id INTEGER PRIMARY KEY, payload TEXT)",
            )
            .expect("table");
        for id in 0..128 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO early_recovery_memory (id,payload) VALUES ({id},'kept')"
                ))
                .expect("insert");
        }
    }
    // Open storage alone. No runtime bootstrap or late schema hydration has
    // run yet, so this pins the memory-saving path to snapshot reconstruction.
    let db = crate::storage::RedDB::open_with_options(&options).expect("cold storage open");
    let manager = db
        .store()
        .get_collection("early_recovery_memory")
        .expect("collection");
    let mut count = 0;
    manager.for_each_entity(|entity| {
        let row = entity.data.as_row().expect("row");
        assert!(
            row.named.is_none(),
            "paged hydration must compact before full materialization"
        );
        assert_eq!(
            row.get_field("payload"),
            Some(&reddb_types::Value::text("kept"))
        );
        count += 1;
        true
    });
    assert_eq!(count, 128);
}

#[test]
fn paged_recovery_keeps_bootstrap_and_declared_schema_metadata_readable() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("bootstrap-recovery-memory.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(crate::StorageDeployPreset::Serverless.selection())
        .expect("paged profile")
        .with_memory_budget(32 * 1024 * 1024);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("create");
        assert_eq!(
            runtime.inner.db.store().set_config_tree(
                "system.bootstrap",
                &crate::serde_json::json!({"completed": true, "preset": "production"}),
            ),
            2
        );
        runtime
            .execute_query("CREATE TABLE bootstrap_recovery (id INTEGER, payload TEXT)")
            .expect("declared schema");
    }
    for _ in 0..3 {
        // Bootstrap reads this before a runtime or any late hydration exists.
        {
            let db = crate::storage::RedDB::open_with_options(&options).expect("cold storage open");
            assert_eq!(
                db.store()
                    .get_config(crate::service_cli::BOOTSTRAP_COMPLETED_KEY),
                Some(reddb_types::Value::Boolean(true)),
                "an existing vault must not be bootstrapped again after recovery"
            );
        }
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime reopen");
        assert_eq!(
            runtime
                .inner
                .db
                .store()
                .get_config(crate::service_cli::BOOTSTRAP_PRESET_KEY),
            Some(reddb_types::Value::text("production"))
        );
        let manager = runtime
            .inner
            .db
            .store()
            .get_collection("bootstrap_recovery")
            .expect("table");
        assert_eq!(
            manager
                .column_schema()
                .expect("rehydrated declared columns")
                .len(),
            2
        );
    }
}

#[test]
fn metadata_publication_memory_does_not_copy_large_unrelated_payloads() {
    use crate::storage::unified::entity::{EntityData, EntityId, RowData, UnifiedEntity};
    use reddb_types::Value;
    let observe = |payload_bytes| {
        let directory = tempfile::tempdir().expect("directory");
        let options = RedDBOptions::persistent(directory.path().join("ddl-memory.rdb"))
            .with_storage_profile(crate::StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
            .with_memory_budget(32 * 1024 * 1024);
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("CREATE TABLE source (id INTEGER, payload BLOB)")
            .expect("source");
        let manager = runtime
            .db()
            .store()
            .get_collection("source")
            .expect("source manager");
        for id in 1..=32 {
            let mut entity =
                UnifiedEntity::table_row(EntityId::new(1000 + id), "source", id, vec![]);
            entity.data = EntityData::Row(RowData::with_names(
                vec![
                    Value::Integer(id as i64),
                    Value::Blob(vec![255; payload_bytes]),
                ],
                vec!["id".into(), "payload".into()],
            ));
            manager.insert(entity).expect("source row");
        }
        runtime
            .checkpoint()
            .expect("publish source before unrelated DDL");
        let (result, allocations) =
            crate::storage::no_malloc_hot_paths::measure_allocations(|| {
                runtime.execute_query("CREATE TABLE unrelated_empty (id INTEGER)")
            });
        result.expect("DDL succeeds independently of unrelated payload size");
        assert_eq!(manager.count(), 32);
        assert_eq!(
            manager
                .get(EntityId::new(1001))
                .expect("source survives")
                .data
                .as_row()
                .expect("row")
                .get_field("payload"),
            Some(&Value::Blob(vec![255; payload_bytes]))
        );
        allocations
    };
    let baseline = observe(0);
    let measured = observe(64 * 1024);
    eprintln!(
        "DDL allocation bytes: {} -> {}",
        baseline.allocated_bytes, measured.allocated_bytes
    );
    assert!(measured.allocated_bytes <= baseline.allocated_bytes + 1024 * 1024,
        "metadata publication must not copy unrelated payloads for derived search artifacts: {baseline:?} -> {measured:?}");
}

#[test]
fn native_artifact_budget_deferral_is_visible_recoverable_and_reversible() {
    let directory = tempfile::tempdir().expect("directory");
    let options = RedDBOptions::persistent(directory.path().join("artifact-budget.rdb"))
        .with_capability(crate::api::Capability::FullText)
        .with_storage_profile(crate::StorageDeployPreset::Serverless.selection())
        .expect("paged profile")
        .with_memory_budget(32 * 1024 * 1024);
    let payload = "alpha beta ".repeat(1024);
    let state = |runtime: &RedDBRuntime| {
        let metadata = runtime
            .db()
            .physical_metadata()
            .expect("published metadata");
        let index = metadata
            .indexes
            .iter()
            .find(|index| {
                index.collection.as_deref() == Some("artifact_budget")
                    && index.artifact_kind.as_deref() == Some("text.fulltext")
            })
            .expect("fulltext index state");
        index.build_state.clone()
    };
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("CREATE DOCUMENT artifact_budget")
            .expect("document collection");
        let manager = runtime
            .db()
            .store()
            .get_collection("artifact_budget")
            .expect("manager");
        for id in 0..32 {
            // A legacy document can retain promoted named fields alongside
            // its canonical JSON body. This exercises both native artifacts.
            let mut entity = crate::storage::UnifiedEntity::table_row(
                crate::storage::EntityId::new(1000 + id),
                "artifact_budget",
                id,
                vec![],
            );
            entity.data = crate::storage::EntityData::Row(
                crate::storage::unified::entity::RowData::with_names(
                    vec![
                        reddb_types::Value::Integer(id as i64),
                        reddb_types::Value::text(payload.as_str()),
                        reddb_types::Value::Json(br#"{"legacy":true}"#.to_vec()),
                    ],
                    vec!["id".into(), "payload".into(), "body".into()],
                ),
            );
            manager.insert(entity).expect("legacy source row");
        }
        runtime.checkpoint().expect("bounded publication");
        assert_eq!(state(&runtime), "deferred-memory-budget");
    }
    let runtime = RedDBRuntime::with_options(options).expect("cold reopen");
    assert_eq!(state(&runtime), "deferred-memory-budget");
    let records = runtime
        .execute_query("SELECT id,payload FROM artifact_budget ORDER BY id")
        .expect("canonical query after deferral")
        .result
        .records;
    assert_eq!(records.len(), 32);
    assert!(records
        .iter()
        .all(|row| row.get("payload") == Some(&reddb_types::Value::text(payload.as_str()))));
    runtime
        .execute_query("DELETE FROM artifact_budget WHERE id >= 1")
        .expect("delete");
    runtime
        .execute_query("VACUUM artifact_budget")
        .expect("reclaim");
    runtime.checkpoint().expect("publish smaller artifact");
    assert_ne!(state(&runtime), "deferred-memory-budget");
    let store = runtime.db().store();
    let header = store.physical_file_header().expect("header");
    assert!(store
        .read_native_vector_artifact_blob(
            header.vector_artifact_page,
            "artifact_budget",
            Some("text.fulltext")
        )
        .expect("artifact read")
        .is_some());
}

#[test]
fn dynamic_recovery_shape_preserves_compiled_filters_and_projections() {
    use crate::storage::unified::entity::{EntityData, EntityId, RowData, UnifiedEntity};
    use reddb_types::Value;
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE dynamic_recovery (id INTEGER)")
        .expect("table");
    let manager = runtime
        .inner
        .db
        .store()
        .get_collection("dynamic_recovery")
        .expect("manager");
    let mut entity = UnifiedEntity::table_row(EntityId::new(1000), "dynamic_recovery", 1, vec![]);
    entity.data = EntityData::Row(RowData {
        columns: vec![],
        named: Some(
            [
                ("a_dimension".into(), Value::Integer(999)),
                ("id".into(), Value::Integer(7)),
                ("z_payload".into(), Value::Integer(42)),
            ]
            .into(),
        ),
        schema: None,
    });
    manager
        .restore_snapshot_entity(entity)
        .expect("recover dynamic row");
    let records = runtime
        .execute_query("SELECT id,z_payload FROM dynamic_recovery WHERE id = 7")
        .expect("compiled read")
        .result
        .records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].get("id"), Some(&Value::Integer(7)));
    assert_eq!(records[0].get("z_payload"), Some(&Value::Integer(42)));
}
