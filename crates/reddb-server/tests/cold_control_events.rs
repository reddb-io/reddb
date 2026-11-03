use reddb_server::runtime::control_events::{
    ActorRef, ControlEvent, ControlEventCtx, ControlEventLedger, EventKind, Outcome, RuntimeLedger,
    Sensitivity,
};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::{borrow::Cow, collections::HashMap};

fn emit(runtime: &RedDBRuntime, action: String, scope: Option<&str>, payload_bytes: usize) {
    RuntimeLedger::new(runtime.db().store().clone())
        .emit(
            &ControlEventCtx {
                actor: ActorRef::Anonymous,
                scope: scope.map(Cow::Borrowed),
                request_id: None,
                trace_id: None,
            },
            ControlEvent {
                kind: EventKind::PolicyCreate,
                outcome: Outcome::Allowed,
                action: Cow::Owned(action),
                resource: None,
                reason: None,
                matched_policy_id: None,
                fields: HashMap::from([(
                    "fixture".into(),
                    Sensitivity::Raw("x".repeat(payload_bytes)),
                )]),
            },
        )
        .expect("durable event");
}

#[test]
fn audit_view_reads_cold_and_new_events_after_reopen() {
    let directory = tempfile::tempdir().expect("fixture");
    let options = RedDBOptions::persistent(directory.path().join("audit.rdb"))
        .with_memory_budget(32 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("create");
        for index in 0..4 {
            emit(&runtime, format!("cold.{index}"), Some("tenant-a"), 128);
        }
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(options).expect("cold reopen");
    let before = runtime
        .execute_query(
            "SELECT action FROM red.control_events WHERE action LIKE 'cold.%' ORDER BY action",
        )
        .expect("audit query");
    assert_eq!(
        before.result.records.len(),
        4,
        "persisted audit rows must remain visible"
    );
    emit(&runtime, "cold.4".into(), Some("tenant-a"), 128);
    let result = runtime.execute_query("SELECT e.action AS event FROM red.control_events AS e WHERE e.action LIKE 'cold.%' ORDER BY e.action DESC LIMIT 2 OFFSET 1").expect("cold and overlay query");
    assert_eq!(
        result
            .result
            .records
            .iter()
            .map(|r| r.get("event").cloned())
            .collect::<Vec<_>>(),
        vec![Some(Value::text("cold.3")), Some(Value::text("cold.2"))]
    );
}

fn cold_fixture(rows: usize, payload_bytes: usize) -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("fixture");
    let options = RedDBOptions::persistent(directory.path().join("audit.rdb"))
        .with_memory_budget(32 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("create");
        for index in 0..rows {
            emit(
                &runtime,
                format!("cold.{index:04}"),
                Some("tenant-a"),
                payload_bytes,
            );
        }
        emit(&runtime, "other-tenant".into(), Some("tenant-b"), 0);
        emit(&runtime, "global".into(), None, 0);
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(options).expect("cold reopen");
    (directory, runtime)
}

fn assert_no_query_buffers(runtime: &RedDBRuntime) {
    use reddb_server::storage::memory_pools::MemoryPool;
    assert_eq!(
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers),
        0
    );
}

#[test]
fn cold_audit_scope_filters_before_limit() {
    use reddb_server::runtime::mvcc::{clear_current_tenant, set_current_tenant};
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            clear_current_tenant();
        }
    }
    let (_directory, runtime) = cold_fixture(3, 128);
    let _scope = Scope;
    set_current_tenant("tenant-a".into());
    let result = runtime
        .execute_query("SELECT scope, action FROM red.control_events ORDER BY action DESC LIMIT 10")
        .expect("tenant query");
    assert_eq!(result.result.records.len(), 3);
    for row in &result.result.records {
        assert_eq!(row.get("scope"), Some(&Value::text("tenant-a")));
    }
    set_current_tenant("tenant-b".into());
    let result = runtime
        .execute_query("SELECT action FROM red.control_events LIMIT 1")
        .expect("other tenant");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("action"),
        Some(&Value::text("other-tenant"))
    );
    set_current_tenant("tenant-empty".into());
    assert!(runtime
        .execute_query("SELECT * FROM red.control_events")
        .expect("empty scope")
        .result
        .records
        .is_empty());
    assert_no_query_buffers(&runtime);
}

#[test]
fn cold_audit_top_window_is_bounded_and_unlimited_reads_obey_cap() {
    let (_directory, runtime) = cold_fixture(1024, 16384);
    runtime
        .execute_query("SET CONFIG stream.executor.max_materialized_rows = 32")
        .expect("small sort cap");
    for _ in 0..3 {
        let result = runtime.execute_query("SELECT action FROM red.control_events WHERE action LIKE 'cold.%' ORDER BY action DESC LIMIT 5 OFFSET 7").expect("bounded window over a larger history");
        let actual: Vec<_> = result
            .result
            .records
            .iter()
            .map(|r| r.get("action").cloned())
            .collect();
        let expected: Vec<_> = (1012..=1016)
            .rev()
            .map(|i| Some(Value::text(format!("cold.{i:04}"))))
            .collect();
        assert_eq!(actual, expected);
        assert_no_query_buffers(&runtime);
    }
    let error = runtime
        .execute_query("SELECT action FROM red.control_events ORDER BY action")
        .expect_err("unlimited query must obey cap");
    assert!(
        matches!(
            error,
            reddb_server::RedDBError::MaterializationLimitExceeded {
                executor: "control_events",
                limit: 32,
                ..
            }
        ),
        "{error:?}"
    );
    assert_no_query_buffers(&runtime);
    let result = runtime
        .execute_query(
            "SELECT action FROM red.control_events WHERE action LIKE 'cold.%' LIMIT 4 OFFSET 1000",
        )
        .expect("stream offset without retaining skipped rows");
    assert_eq!(result.result.records.len(), 4);
    assert!(runtime
        .execute_query("SELECT * FROM red.control_events LIMIT 0")
        .expect("zero limit")
        .result
        .records
        .is_empty());
    assert!(runtime
        .execute_query("SELECT * FROM red.control_events LIMIT 1 OFFSET 100000")
        .expect("past end")
        .result
        .records
        .is_empty());
    assert_no_query_buffers(&runtime);
    runtime
        .execute_query("SET CONFIG stream.executor.max_materialized_rows = 0")
        .expect("disable row cap only");
    let error = runtime
        .execute_query("SELECT * FROM red.control_events ORDER BY action")
        .expect_err("growing workspace must still obey the shared byte budget");
    assert!(
        matches!(
            error,
            reddb_server::RedDBError::MaterializationLimitExceeded {
                executor: "control_events",
                limit: 33_554_432,
                ..
            }
        ),
        "{error:?}"
    );
    assert_no_query_buffers(&runtime);
    assert_eq!(
        runtime
            .execute_query("SELECT action FROM red.control_events ORDER BY action LIMIT 1")
            .expect("failed sort returned its reservation")
            .result
            .records
            .len(),
        1
    );
    assert_no_query_buffers(&runtime);
}

#[test]
fn cold_audit_rejects_oversized_decode_and_releases_scratch() {
    let (_directory, runtime) = cold_fixture(1, 3 * 1024 * 1024);
    let error = runtime
        .execute_query("SELECT * FROM red.control_events")
        .expect_err("row exceeds decode allowance");
    let message = error.to_string();
    assert!(
        message.contains("budget") || message.contains("limit"),
        "{message}"
    );
    assert_no_query_buffers(&runtime);
    // LIMIT 0 must not decode the oversized row.
    assert!(runtime
        .execute_query("SELECT * FROM red.control_events LIMIT 0")
        .expect("zero limit")
        .result
        .records
        .is_empty());
}

#[test]
fn cold_audit_does_not_hide_corrupt_backing_pages() {
    use reddb_engine::storage::page::{Page, PageType};
    let (_directory, runtime) = cold_fixture(3, 128);
    let manager = runtime
        .db()
        .store()
        .get_collection("red.control_events")
        .expect("ledger");
    let root = manager.backing_root_page().expect("cold root");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let original = pager.read_page(root).expect("original fixture page");
    pager
        .write_page(root, Page::new(PageType::Overflow, root))
        .expect("inject wrong page type in disposable fixture");
    let result = runtime.execute_query("SELECT * FROM red.control_events");
    pager
        .write_page(root, original)
        .expect("restore fixture before shutdown");
    let error = result.expect_err("corruption must not look like an empty ledger");
    assert!(error.to_string().to_lowercase().contains("page"), "{error}");
    assert_no_query_buffers(&runtime);
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM red.control_events")
            .expect("restored fixture")
            .result
            .records
            .len(),
        5
    );
}

#[test]
fn cold_audit_shares_admission_with_other_queries() {
    use reddb_server::storage::memory_pools::MemoryPool;
    let (_directory, runtime) = cold_fixture(3, 128);
    let accounting = runtime.memory_accounting();
    let occupied = runtime.memory_budget().resolved_bytes;
    accounting.charge(MemoryPool::QueryBuffers, occupied);
    let result = runtime.execute_query("SELECT * FROM red.control_events");
    assert_eq!(
        accounting.used_bytes(MemoryPool::QueryBuffers),
        occupied,
        "failed query must not leak or release another owner"
    );
    accounting.release(MemoryPool::QueryBuffers, occupied);
    let error = result.expect_err("shared pool exhaustion");
    assert!(
        matches!(
            error,
            reddb_server::RedDBError::MaterializationLimitExceeded { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM red.control_events")
            .expect("headroom returned")
            .result
            .records
            .len(),
        5
    );
    assert_no_query_buffers(&runtime);
}

#[test]
fn newest_audit_page_reads_only_its_index_candidates() {
    use reddb_engine::storage::collection_read::{
        CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
    };
    use reddb_server::storage::{EntityId, StoreError};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct NoScan {
        inner: Arc<dyn CollectionRead + Send + Sync>,
        reads: Arc<AtomicUsize>,
    }
    impl CollectionRead for NoScan {
        fn get_record(
            &self,
            id: EntityId,
            limit: usize,
        ) -> Result<Option<EncodedRecord>, StoreError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.inner.get_record(id, limit)
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
            Err(StoreError::Serialization(
                "audit page must not scan the whole ledger".into(),
            ))
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
            Err(StoreError::Serialization(
                "audit page must not scan the whole ledger".into(),
            ))
        }
    }
    let (_directory, runtime) = cold_fixture(128, 128);
    // Extra unique-key tie breaker forces the generic scan for the reference.
    let reference = runtime.execute_query("SELECT id, ts, action FROM red.control_events ORDER BY ts DESC, id DESC, action DESC LIMIT 7 OFFSET 3").expect("reference scan");
    let store = runtime.db().store();
    let root = store
        .get_collection("red.control_events")
        .expect("ledger")
        .backing_root_page()
        .expect("root");
    let reads = Arc::new(AtomicUsize::new(0));
    let manager = store.get_collection("red.control_events").expect("ledger");
    manager.install_backing(
        Arc::new(NoScan {
            inner: Arc::new(PagedCollection::new(Arc::new(
                reddb_server::storage::engine::BTree::with_root(
                    Arc::clone(store.pager().expect("pager")),
                    root,
                ),
            ))),
            reads: Arc::clone(&reads),
        }),
        root,
        store.format_version(),
        2 * 1024 * 1024,
        130,
    );
    let result = runtime.execute_query("SELECT id, ts, action FROM red.control_events ORDER BY ts DESC, id DESC LIMIT 7 OFFSET 3").expect("bounded index read instead of a complete scan");
    assert_eq!(result.result.records.len(), reference.result.records.len());
    for (actual, expected) in result.result.records.iter().zip(&reference.result.records) {
        for column in ["id", "ts", "action"] {
            assert_eq!(actual.get(column), expected.get(column));
        }
    }
    assert!(
        reads.load(Ordering::Relaxed) <= 10,
        "read only the requested base window"
    );
    emit(&runtime, "hot.after-reopen".into(), Some("tenant-a"), 128);
    reads.store(0, Ordering::Relaxed);
    let hot = runtime
        .execute_query(
            "SELECT e.action AS action FROM red.control_events AS e ORDER BY e.ts DESC, e.id DESC LIMIT 1",
        )
        .expect("merge new overlay without scanning base");
    assert_eq!(
        hot.result.records[0].get("action"),
        Some(&Value::text("hot.after-reopen"))
    );
    assert!(reads.load(Ordering::Relaxed) <= 1);
    assert_no_query_buffers(&runtime);
}

fn assert_latest_matches_scan(runtime: &RedDBRuntime) {
    use reddb_server::runtime::mvcc::{clear_current_tenant, set_current_tenant};
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            clear_current_tenant();
        }
    }
    let _scope = Scope;
    for tenant in [None, Some("tenant-a"), Some("tenant-b"), Some("empty")] {
        clear_current_tenant();
        if let Some(tenant) = tenant {
            set_current_tenant(tenant.into());
        }
        for filter in [
            "",
            "WHERE action LIKE 'cold.%'",
            "WHERE ts > 0 AND action = 'other-tenant'",
        ] {
            let prefix = format!(
                "SELECT id, ts, action FROM red.control_events {filter} ORDER BY ts DESC, id DESC"
            );
            let expected = runtime
                .execute_query(&format!("{prefix}, action DESC LIMIT 7 OFFSET 1"))
                .expect("full-scan reference");
            let actual = runtime
                .execute_query(&format!("{prefix} LIMIT 7 OFFSET 1"))
                .expect("indexed page");
            assert_eq!(actual.result.records.len(), expected.result.records.len());
            for (actual, expected) in actual.result.records.iter().zip(&expected.result.records) {
                for key in ["id", "ts", "action"] {
                    assert_eq!(actual.get(key), expected.get(key));
                }
            }
        }
    }
    assert_no_query_buffers(runtime);
}

#[test]
fn audit_index_keeps_overlay_deletes_filters_and_checkpoint_reopens() {
    let (directory, runtime) = cold_fixture(96, 128);
    let options = RedDBOptions::persistent(directory.path().join("audit.rdb"))
        .with_memory_budget(32 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    let store = runtime.db().store();
    let manager = store.get_collection("red.control_events").expect("ledger");
    let mut removed = None;
    manager
        .try_scan_for_each(None, 2 * 1024 * 1024, |row| {
            removed = Some(row.id);
            false
        })
        .expect("source row");
    store
        .delete("red.control_events", removed.expect("row"))
        .expect("delete base row");
    emit(
        &runtime,
        "cold.new-after-delete".into(),
        Some("tenant-a"),
        128,
    );
    assert_latest_matches_scan(&runtime);
    runtime.checkpoint().expect("publish deletion and overlay");
    drop(manager);
    drop(store);
    drop(runtime);
    for round in 0..3 {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("prepared reopen");
        assert_latest_matches_scan(&runtime);
        emit(
            &runtime,
            format!("cold.sparse-{round}"),
            Some("tenant-b"),
            128,
        );
        assert_latest_matches_scan(&runtime);
        runtime
            .checkpoint()
            .expect("sparse append maintains native postings");
    }
}

#[test]
fn audit_index_falls_back_for_legacy_ids_in_overlay_and_on_disk() {
    use reddb_server::storage::{EntityData, EntityId};
    let (directory, runtime) = cold_fixture(8, 128);
    let store = runtime.db().store();
    let manager = store.get_collection("red.control_events").expect("ledger");
    let mut legacy = None;
    manager
        .try_scan_for_each(None, 2 * 1024 * 1024, |row| {
            legacy = Some(row.clone());
            false
        })
        .expect("fixture row");
    let mut legacy = legacy.expect("row");
    legacy.id = EntityId::new(0);
    let EntityData::Row(row) = &mut legacy.data else {
        panic!("row")
    };
    let fields = row.named.as_mut().expect("named ledger fields");
    fields.insert("id".into(), Value::text("legacy-short-id"));
    fields.insert("ts".into(), Value::Integer(i64::MAX));
    store
        .insert_auto("red.control_events", legacy)
        .expect("legacy row");
    let sql = "SELECT id FROM red.control_events ORDER BY ts DESC, id DESC LIMIT 1";
    assert_eq!(
        runtime
            .execute_query(sql)
            .expect("overlay fallback")
            .result
            .records[0]
            .get("id"),
        Some(&Value::text("legacy-short-id"))
    );
    runtime.checkpoint().expect("publish fallback marker");
    drop(manager);
    drop(store);
    drop(runtime);
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(directory.path().join("audit.rdb"))
            .with_memory_budget(32 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .expect("reopen marker");
    assert_eq!(
        runtime
            .execute_query(sql)
            .expect("persisted fallback")
            .result
            .records[0]
            .get("id"),
        Some(&Value::text("legacy-short-id"))
    );
    assert_no_query_buffers(&runtime);
}

#[test]
fn crash_reopen_with_new_collection_keeps_prepared_audit_roots() {
    const CHILD: &str = "REDDB_AUDIT_NEW_COLLECTION_CRASH_PATH";
    if let Some(path) = std::env::var_os(CHILD) {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .expect("child reopen");
        runtime
            .execute_query("CREATE KV audit_recovery_pending_count")
            .expect("acknowledged new collection");
        runtime
            .execute_query("KV PUT audit_recovery_pending_count.marker = 'acknowledged'")
            .expect("acknowledged KV");
        emit(&runtime, "cold.after-crash".into(), Some("tenant-a"), 128);
        std::process::exit(71); // No destructor checkpoint after acknowledged writes.
    }
    let (directory, runtime) = cold_fixture(2048, 2048);
    runtime
        .execute_query("CREATE TABLE audit_boot_catalog (id INT, bucket INT)")
        .expect("catalog table");
    runtime
        .execute_query("CREATE INDEX audit_boot_bucket ON audit_boot_catalog (bucket) USING BTREE")
        .expect("persisted index registry");
    runtime
        .checkpoint()
        .expect("prepared ledger with a runtime index registry");
    let root = runtime
        .db()
        .store()
        .get_collection("red.control_events")
        .expect("ledger")
        .backing_root_page()
        .expect("prepared root");
    drop(runtime);
    let path = directory.path().join("audit.rdb");
    let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "crash_reopen_with_new_collection_keeps_prepared_audit_roots",
            "--nocapture",
        ])
        .env(CHILD, &path)
        .output()
        .expect("crash fixture process");
    assert_eq!(
        child.status.code(),
        Some(71),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(path)
            .with_memory_budget(32 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .expect("recover pending new collection");
    let recovered_root = runtime
        .db()
        .store()
        .get_collection("red.control_events")
        .expect("ledger")
        .backing_root_page()
        .expect("recovered root");
    assert_eq!(recovered_root, root, "a missing projection on a new collection must not rewrite historical audit data before its projector is registered");
    assert_eq!(
        runtime
            .execute_query("KV GET audit_recovery_pending_count.marker")
            .expect("recovered KV")
            .result
            .records[0]
            .get("value"),
        Some(&Value::text("acknowledged"))
    );
    assert_eq!(runtime.execute_query("SELECT action FROM red.control_events WHERE action = 'cold.after-crash' ORDER BY ts DESC,id DESC LIMIT 1").expect("recovered audit").result.records.len(), 1);
}
