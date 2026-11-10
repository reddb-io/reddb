use super::*;
use crate::RedDBOptions;
use reddb_engine::storage::collection_read::{CollectionCursor, CollectionRead, EncodedRecord};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct PointReadsOnly {
    rows: HashMap<EntityId, Vec<u8>>,
    reads: AtomicUsize,
    fail: AtomicBool,
}

impl CollectionRead for PointReadsOnly {
    fn get_record(&self, id: EntityId, _: usize) -> Result<Option<EncodedRecord>, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(StoreError::Serialization(
                "injected point-read failure".into(),
            ));
        }
        Ok(self.rows.get(&id).map(|bytes| EncodedRecord {
            id,
            bytes: bytes.clone(),
        }))
    }
    fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
        Err(StoreError::Serialization("unexpected payload scan".into()))
    }
    fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
        Err(StoreError::Serialization("unexpected payload scan".into()))
    }
}

fn deny_scans(runtime: &RedDBRuntime, collection: &str) -> Arc<PointReadsOnly> {
    let store = runtime.db().store();
    let manager = store.get_collection(collection).expect("collection");
    let root = manager.backing_root_page().expect("cold backing");
    let count = manager.backing_count_at_root(root).expect("count");
    let mut rows = HashMap::new();
    manager
        .try_for_each_entity(64 * 1024 * 1024, |entity| {
            rows.insert(
                entity.id,
                reddb_engine::storage::metadata_codec::serialize_entity_record(
                    entity,
                    None,
                    store.format_version(),
                ),
            );
            true
        })
        .expect("capture fixture");
    let source = Arc::new(PointReadsOnly {
        rows,
        reads: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    manager.install_backing(
        source.clone(),
        root,
        store.format_version(),
        64 * 1024 * 1024,
        count,
    );
    source
}

fn open(options: &RedDBOptions) -> RedDBRuntime {
    crate::storage::RedDB::with_disk_residency_for_qualification(|| {
        RedDBRuntime::with_options(options.clone())
    })
    .expect("cold runtime")
}

#[test]
fn dedup_expiry_backlog_is_bounded_and_does_not_extend_the_window() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let store = runtime.db().store();
    for id in 1..=200 {
        crate::runtime::impl_queue::record_queue_dedup(
            &store,
            "jobs",
            &format!("key-{id}"),
            EntityId::new(id),
            100,
        )
        .expect("dedup row");
    }
    assert_eq!(
        crate::runtime::impl_queue::reclaim_expired_queue_dedup(&store, "jobs", 100)
            .expect("first bounded cleanup"),
        64
    );
    assert!(
        crate::runtime::impl_queue::find_queue_dedup(&store, "jobs", "key-200", 100)
            .expect("expired lookup")
            .is_none()
    );
    let mut reclaimed = 64;
    loop {
        let count = crate::runtime::impl_queue::reclaim_expired_queue_dedup(&store, "jobs", 100)
            .expect("cleanup");
        assert!(count <= 64);
        reclaimed += count;
        if count == 0 {
            break;
        }
    }
    assert_eq!(reclaimed, 200);
}

#[test]
fn cold_dedup_and_capacity_do_not_scan_payloads_and_propagate_read_errors() {
    let directory = tempfile::tempdir().expect("directory");
    let options = RedDBOptions::persistent(directory.path().join("queues.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_storage_profile(
            crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
        )
        .expect("profile");
    let runtime = RedDBRuntime::with_options(options.clone()).expect("seed");
    runtime
        .execute_query("CREATE QUEUE jobs MAX_SIZE 2")
        .expect("queue");
    runtime
        .execute_query("QUEUE PUSH jobs 'first'")
        .expect("first");
    runtime
        .execute_query("QUEUE PUSH jobs 'second'")
        .expect("second");
    let store = runtime.db().store();
    for id in 1..=160 {
        crate::runtime::impl_queue::record_queue_dedup(
            &store,
            "jobs",
            &format!("expired-{id}"),
            EntityId::new(id),
            100,
        )
        .expect("expired row");
        crate::runtime::impl_queue::record_queue_dedup(
            &store,
            "other",
            &format!("expired-{id}"),
            EntityId::new(id),
            100,
        )
        .expect("other queue");
    }
    crate::runtime::impl_queue::record_queue_dedup(
        &store,
        "jobs",
        "live",
        EntityId::new(999),
        u64::MAX,
    )
    .expect("live dedup");
    runtime.flush().expect("checkpoint");
    drop(store);
    drop(runtime);
    let runtime = open(&options);
    let metadata = deny_scans(&runtime, META_COLLECTION);
    let messages = deny_scans(&runtime, "jobs");
    let store = runtime.db().store();
    assert_eq!(
        crate::runtime::impl_queue::find_queue_dedup(&store, "jobs", "live", 100)
            .expect("indexed lookup"),
        Some(EntityId::new(999))
    );
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
    assert!(
        crate::runtime::impl_queue::find_queue_dedup(&store, "jobs", "absent", 100)
            .expect("indexed miss")
            .is_none()
    );
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
    let expired = dedup_rows(&store, "jobs", None, 100, 64).expect("bounded expiration");
    assert_eq!(expired.len(), 64);
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 65);
    let next = dedup_rows(&store, "jobs", None, 100, 64).expect("next bounded page");
    assert_eq!(next.len(), 64);
    assert!(
        next.iter().all(|row| !expired.contains(row)),
        "equal deadlines must resume after physical identity"
    );
    let tail = dedup_rows(&store, "jobs", None, 100, 64).expect("last page");
    assert_eq!(tail.len(), 32);
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 161);
    // A completed sweep wraps, so a failed delete or a newly expired older
    // record is retried; the cursor is never the source of truth.
    assert_eq!(
        dedup_rows(&store, "jobs", None, 100, 64)
            .expect("wrap")
            .len(),
        64
    );
    assert_eq!(
        capacity_reached(&runtime, "jobs", 2).expect("capacity"),
        Some(true)
    );
    assert_eq!(
        capacity_reached(&runtime, "jobs", 3).expect("capacity"),
        Some(false)
    );
    assert_eq!(messages.reads.load(Ordering::Relaxed), 0);
    metadata.fail.store(true, Ordering::Relaxed);
    assert!(crate::runtime::impl_queue::find_queue_dedup(&store, "jobs", "live", 100).is_err());
}
