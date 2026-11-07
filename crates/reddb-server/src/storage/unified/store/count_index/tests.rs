use super::*;
use reddb_engine::transaction::snapshot::SnapshotManager;

fn config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(32),
        resident_memory_bytes_max: Some(2 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    }
}

fn fixture() -> (tempfile::TempDir, UnifiedStore) {
    let directory = tempfile::tempdir().expect("visibility fixture");
    let store =
        UnifiedStore::open_with_config(directory.path().join("count.rdb"), config()).expect("open");
    store.create_collection("rows").expect("collection");
    (directory, store)
}

#[test]
fn count_batch_limits_mixed_keys_and_groups_uniform_keys() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountVisits(Arc<AtomicUsize>);
    impl SecondaryIndexProjector for CountVisits {
        fn project(
            &self,
            entity: &UnifiedEntity,
            key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
        ) -> Result<Option<usize>, StoreError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            VisibilityProjection { schema: None }.project(entity, key)
        }
    }
    for (mode, expected_visits) in [
        ("uniform", UNIFORM_BATCH_ROWS),
        ("mixed", MIXED_BATCH_ROWS),
        ("boundary", MIXED_BATCH_ROWS + 1),
    ] {
        let (_directory, store) = fixture();
        let rows = UNIFORM_BATCH_ROWS as u64 + 1;
        store
            .get_collection("rows")
            .unwrap()
            .bulk_insert(
                (1..=rows)
                    .map(|id| {
                        let mut entity =
                            UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]);
                        entity.xmin = match mode {
                            "mixed" => id % 2,
                            "boundary" if id > MIXED_BATCH_ROWS as u64 => 1,
                            _ => 0,
                        };
                        entity
                    })
                    .collect(),
            )
            .unwrap();
        store.persist().unwrap();
        store.register_count_index_deferred("rows").unwrap();
        let mut build = store.start_count_build("rows").unwrap().unwrap();
        let visits = Arc::new(AtomicUsize::new(0));
        build.projector = Arc::new(CountVisits(Arc::clone(&visits)));
        assert!(!build.step(&store).unwrap());
        assert_eq!(visits.load(Ordering::Relaxed), expected_visits);
        while !build.step(&store).unwrap() {}
        compare(&store, None, rows);
    }
}

#[test]
fn count_batch_does_not_hold_publication_lock_while_decoding() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    };
    use std::time::Duration;
    struct PausedProjection {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        first: AtomicBool,
    }
    impl SecondaryIndexProjector for PausedProjection {
        fn project(
            &self,
            entity: &UnifiedEntity,
            key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
        ) -> Result<Option<usize>, StoreError> {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            VisibilityProjection { schema: None }.project(entity, key)
        }
    }
    let (_directory, store) = fixture();
    let store = Arc::new(store);
    let rows = UNIFORM_BATCH_ROWS as u64 + 1;
    store
        .get_collection("rows")
        .unwrap()
        .bulk_insert(
            (1..=rows)
                .map(|id| UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]))
                .collect(),
        )
        .unwrap();
    store.persist().unwrap();
    store.register_count_index_deferred("rows").unwrap();
    let mut build = store.start_count_build("rows").unwrap().unwrap();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    build.projector = Arc::new(PausedProjection {
        entered,
        release: Mutex::new(release_rx),
        first: AtomicBool::new(true),
    });
    let builder_store = Arc::clone(&store);
    let builder = std::thread::spawn(move || {
        let result = build.step(&builder_store);
        (build, result)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("builder reached source scan");
    let writer_store = Arc::clone(&store);
    let (written, written_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let read = writer_store
            .try_get("rows", EntityId::new(1), 1024 * 1024)
            .unwrap()
            .is_some();
        let write = writer_store.insert(
            "rows",
            UnifiedEntity::table_row(EntityId::new(rows + 1), "rows", rows + 1, vec![]),
        );
        written.send((read, write.is_ok())).unwrap();
    });
    let progress = written_rx.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    writer.join().unwrap();
    let (mut build, result) = builder.join().unwrap();
    assert_eq!(
        progress.expect("reads and writes must not wait for the source scan"),
        (true, true)
    );
    assert!(!result.unwrap());
    while !build.step(&store).unwrap() {}
    compare(&store, None, rows + 1);
}

#[test]
fn incremental_count_build_allows_writes_and_restarts_after_checkpoint() {
    let (directory, store) = fixture();
    let rows = UNIFORM_BATCH_ROWS as u64 * 3;
    store
        .get_collection("rows")
        .unwrap()
        .bulk_insert(
            (1..=rows)
                .map(|id| {
                    UnifiedEntity::table_row(
                        EntityId::new(id),
                        "rows",
                        id,
                        vec![Value::text("data")],
                    )
                })
                .collect(),
        )
        .unwrap();
    store.persist().unwrap();
    store.register_count_index_deferred("rows").unwrap();
    let mut build = store.start_count_build("rows").unwrap().unwrap();
    assert!(!build.step(&store).unwrap());
    assert_eq!(store.try_count_from_index("rows", None).unwrap(), None);
    // Writes and reads proceed between batches while the partial tree is
    // durable but invisible. A changed data root invalidates the builder.
    store
        .insert(
            "rows",
            UnifiedEntity::table_row(EntityId::new(rows + 1), "rows", rows + 1, vec![]),
        )
        .unwrap();
    assert!(store
        .try_get("rows", EntityId::new(rows + 1), 1024 * 1024)
        .unwrap()
        .is_some());
    store.persist().unwrap();
    assert!(
        build.step(&store).unwrap(),
        "replaced generation must cancel the old build"
    );
    drop(build);
    let mut build = store.start_count_build("rows").unwrap().unwrap();
    while !build.step(&store).unwrap() {}
    compare(&store, None, rows + 1);
    drop(build);
    // Completed batch epochs remain in the WAL until the next checkpoint.
    // A subsequent acknowledged write must still replay after restart.
    store
        .insert(
            "rows",
            UnifiedEntity::table_row(EntityId::new(rows + 2), "rows", rows + 2, vec![]),
        )
        .unwrap();
    compare(&store, None, rows + 2);
    drop(store);
    let reopened =
        UnifiedStore::open_with_config(directory.path().join("count.rdb"), config()).unwrap();
    compare(&reopened, None, rows + 2);
    reopened.register_count_index_deferred("rows").unwrap();
    reopened
        .insert(
            "rows",
            UnifiedEntity::table_row(EntityId::new(rows + 3), "rows", rows + 3, vec![]),
        )
        .unwrap();
    reopened.persist().unwrap();
    if reopened
        .try_count_from_index("rows", None)
        .unwrap()
        .is_none()
    {
        let mut retry = reopened.start_count_build("rows").unwrap().unwrap();
        while !retry.step(&reopened).unwrap() {}
    }
    compare(&reopened, None, rows + 3);
}

#[test]
fn incremental_count_replacement_retires_the_old_definition() {
    let (_directory, store) = fixture();
    store
        .insert(
            "rows",
            UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![]),
        )
        .unwrap();
    store.register_count_index("rows").unwrap();
    store.persist().unwrap();
    let old = Arc::clone(
        &store
            .key_indices
            .read()
            .get("rows")
            .unwrap()
            .secondary
            .get(NAME)
            .unwrap()
            .tree,
    );
    store
        .get_collection("rows")
        .unwrap()
        .set_column_schema_if_empty(vec!["value".into()]);
    store.register_count_index_deferred("rows").unwrap();
    let mut build = store.start_count_build("rows").unwrap().unwrap();
    while !build.step(&store).unwrap() {}
    assert!(store
        .retired_btrees
        .lock()
        .iter()
        .any(|entry| Arc::ptr_eq(&entry.tree, &old)));
    compare(&store, None, 1);
}

#[test]
fn incremental_count_handles_out_of_order_visibility_keys_across_leaves() {
    let (directory, store) = fixture();
    let manager = store.get_collection("rows").unwrap();
    let schema = Arc::new(vec!["__moderation_status".to_owned()]);
    manager.set_column_schema_if_empty(schema.as_ref().clone());
    let mut expected = 0u64;
    let entities = (1..=1301)
        .map(|id| {
            let hidden = id % 7 == 0;
            let deleted = id % 5 == 0;
            expected += u64::from(!hidden && !deleted);
            let mut row = UnifiedEntity::table_row(
                EntityId::new(id),
                "rows",
                id,
                vec![Value::text(if hidden { "pending" } else { "" })],
            );
            row.xmin = (id % 13) * 3;
            row.xmax = if deleted { id % 11 + 1 } else { 0 };
            if let EntityData::Row(data) = &mut row.data {
                data.schema = Some(Arc::clone(&schema));
            }
            row
        })
        .collect();
    manager.bulk_insert(entities).unwrap();
    store.persist().unwrap();
    store.register_count_index_deferred("rows").unwrap();
    let mut build = store.start_count_build("rows").unwrap().unwrap();
    while !build.step(&store).unwrap() {}
    compare(&store, None, expected);
    drop(build);
    drop(manager);
    drop(store);
    let reopened =
        UnifiedStore::open_with_config(directory.path().join("count.rdb"), config()).unwrap();
    reopened
        .get_collection("rows")
        .unwrap()
        .set_column_schema_if_empty(schema.as_ref().clone());
    compare(&reopened, None, expected);
}

#[test]
fn interrupted_count_batch_preserves_rows_and_can_rebuild() {
    const CHILD: &str = "REDDB_TEST_COUNT_BATCH_PATH";
    if let Some(path) = std::env::var_os(CHILD) {
        let store = UnifiedStore::open_with_config(path, config()).unwrap();
        store.register_count_index_deferred("rows").unwrap();
        let mut build = store.start_count_build("rows").unwrap().unwrap();
        assert!(
            !build.step(&store).unwrap(),
            "publish one recoverable partial batch"
        );
        let stage = std::env::var("REDDB_TEST_COUNT_BATCH_STAGE").unwrap();
        let rows: u64 = std::env::var("REDDB_TEST_COUNT_BATCH_ROWS")
            .unwrap()
            .parse()
            .unwrap();
        if stage == "finished" {
            while !build.step(&store).unwrap() {}
            store
                .insert(
                    "rows",
                    UnifiedEntity::table_row(EntityId::new(rows + 1), "rows", rows + 1, vec![]),
                )
                .unwrap();
            std::process::exit(185);
        }
        if stage == "final" {
            assert!(!build.step(&store).unwrap());
        }
        let failpoint = if stage == "before" || stage == "split" {
            "REDDB_TEST_EXIT_DURING_COUNT_BATCH"
        } else {
            "REDDB_TEST_EXIT_AFTER_COUNT_BATCH_PUBLICATION"
        };
        if stage == "split" {
            std::env::set_var("REDDB_TEST_REQUIRE_COUNT_SPLIT", "1");
        }
        std::env::set_var(failpoint, "1");
        let _ = build.step(&store);
        panic!("crash injection did not fire");
    }
    for (stage, exit_code) in [
        ("before", 183),
        ("after", 184),
        ("final", 184),
        ("finished", 185),
        ("split", 183),
    ] {
        let (directory, store) = fixture();
        let rows = UNIFORM_BATCH_ROWS as u64 * 2 + u64::from(stage == "split");
        store
            .get_collection("rows")
            .unwrap()
            .bulk_insert(
                (1..=rows)
                    .map(|id| {
                        let mut entity =
                            UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]);
                        // Make the first batch small so the following full batch
                        // splits its already-published root before the crash.
                        if stage == "split" && id == 1 {
                            entity.xmin = 1;
                        }
                        entity
                    })
                    .collect(),
            )
            .unwrap();
        store.persist().unwrap();
        drop(store);
        let path = directory.path().join("count.rdb");
        let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "storage::unified::store::count_index::tests::interrupted_count_batch_preserves_rows_and_can_rebuild", "--nocapture"])
        .env(CHILD, &path).env("REDDB_TEST_COUNT_BATCH_ROWS", rows.to_string()).env("REDDB_TEST_COUNT_BATCH_STAGE", stage).output().unwrap();
        assert_eq!(result.status.code(), Some(exit_code), "crash stage {stage}");
        let store = UnifiedStore::open_with_config(&path, config()).unwrap();
        let expected = (rows + u64::from(stage == "finished")) as usize;
        assert_eq!(store.get_collection("rows").unwrap().count(), expected);
        let projected = store.try_count_from_index("rows", None).unwrap();
        if stage == "final" || stage == "finished" {
            assert_eq!(projected, Some(expected as u64));
        } else {
            assert_eq!(projected, None);
            store.register_count_index_deferred("rows").unwrap();
            let mut build = store.start_count_build("rows").unwrap().unwrap();
            while !build.step(&store).unwrap() {}
        }
        compare(&store, None, expected as u64);
    }
}

fn context(manager: &Arc<SnapshotManager>) -> SnapshotContext {
    SnapshotContext {
        snapshot: manager.fresh_read_snapshot(),
        manager: Arc::clone(manager),
        own_xids: Default::default(),
        requires_index_fallback: true,
        serializable_reader: None,
    }
}

fn compare(store: &UnifiedStore, context: Option<&SnapshotContext>, expected: u64) {
    let mut scanned = 0;
    store
        .get_collection("rows")
        .expect("rows")
        .try_scan_for_each(context, usize::MAX, |_| {
            scanned += 1;
            true
        })
        .expect("reference scan");
    assert_eq!(scanned, expected, "reference");
    assert_eq!(
        store
            .try_count_from_index("rows", context)
            .expect("indexed"),
        Some(expected)
    );
}

#[test]
fn paged_count_snapshot_active_aborted_and_own_versions_match_scan() {
    let (_directory, store) = fixture();
    let transactions = Arc::new(SnapshotManager::new());
    let before = context(&transactions);
    let committed = transactions.begin();
    transactions.commit(committed);
    let aborted = transactions.begin();
    transactions.rollback(aborted);
    let active = transactions.begin();
    let states = [
        (0, 0),
        (committed, 0),
        (committed, 0),
        (aborted, 0),
        (active, 0),
        (0, committed),
        (0, aborted),
        (0, active),
        (0, active),
    ];
    for (offset, (xmin, xmax)) in states.into_iter().enumerate() {
        let mut entity =
            UnifiedEntity::table_row(EntityId::new(offset as u64 + 1), "rows", 0, vec![]);
        entity.xmin = xmin;
        entity.xmax = xmax;
        store.insert("rows", entity).expect("row");
    }
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    let reader = context(&transactions);
    compare(&store, Some(&before), 5);
    compare(&store, Some(&reader), 6);
    let mut own = reader.clone();
    own.own_xids.insert(active);
    compare(&store, Some(&own), 5);
    transactions.rollback(active);
    compare(&store, Some(&own), 6);
    compare(&store, Some(&reader), 6);
    compare(&store, None, 5);
    let mut serializable = reader;
    serializable.serializable_reader = Some(transactions.begin());
    assert_eq!(
        store
            .try_count_from_index("rows", Some(&serializable))
            .expect("fallback"),
        None
    );
}

#[test]
fn paged_count_moderation_positional_schema_overlay_delete_and_reopen() {
    let (directory, store) = fixture();
    let manager = store.get_collection("rows").expect("rows");
    let schema = vec!["payload".into(), "__moderation_status".into()];
    manager.set_column_schema_if_empty(schema.clone());
    for (offset, marker) in ["", "pending", "rejected"].into_iter().enumerate() {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(offset as u64 + 1),
            "rows",
            0,
            vec![Value::text("big".repeat(8192)), Value::text(marker)],
        );
        if let EntityData::Row(row) = &mut entity.data {
            row.schema = Some(Arc::new(schema.clone()));
        }
        store.insert("rows", entity).expect("row");
    }
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    compare(&store, None, 1);
    let mut cleared = manager
        .try_get(EntityId::new(2), usize::MAX)
        .expect("read")
        .expect("row");
    if let EntityData::Row(row) = &mut cleared.data {
        if let Some(named) = &mut row.named {
            named.insert("__moderation_status".into(), Value::text(""));
        } else {
            row.columns[1] = Value::text("");
        }
    }
    manager
        .update(cleared)
        .expect("clear moderation in overlay");
    compare(&store, None, 2);
    store
        .delete("rows", EntityId::new(1))
        .expect("physical delete");
    compare(&store, None, 1);
    store.persist().expect("publish changes");
    compare(&store, None, 1);
    drop(manager);
    drop(store);
    let store = UnifiedStore::open_with_config(directory.path().join("count.rdb"), config())
        .expect("reopen");
    store
        .get_collection("rows")
        .expect("rows")
        .set_column_schema_if_empty(schema);
    store
        .register_count_index("rows")
        .expect("compatible projector");
    compare(&store, None, 1);
}

#[test]
fn paged_count_multiple_leaves_faults_and_missing_generation() {
    use crate::storage::engine::{Page, PageType};
    let (_directory, store) = fixture();
    for id in 1..=1301 {
        store
            .insert(
                "rows",
                UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]),
            )
            .expect("row");
    }
    assert_eq!(
        store
            .try_count_from_index("rows", None)
            .expect("unprepared"),
        None
    );
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    compare(&store, None, 1301);
    let native = store
        .secondary_index("rows", NAME, &identity(None))
        .expect("projection");
    let pager = store.pager().expect("pager");
    let root = native.root_page_id();
    let original = pager.read_page(root).expect("original");
    pager
        .write_page(root, Page::new(PageType::Overflow, root))
        .expect("inject corrupt index");
    assert!(
        store.try_count_from_index("rows", None).is_err(),
        "unreadable index must not return a partial/zero count"
    );
    pager.write_page(root, original).expect("repair fixture");
    compare(&store, None, 1301);
    store.unregister_secondary_index("rows", NAME);
    assert_eq!(
        store.try_count_from_index("rows", None).expect("missing"),
        None
    );
}

#[test]
fn paged_count_commit_changes_new_snapshots_without_rebuilding_index() {
    let (_directory, store) = fixture();
    store.register_count_index("rows").expect("projection");
    store.persist().expect("empty");
    assert_eq!(
        store
            .try_count_from_index("rows", None)
            .expect("empty scan fallback"),
        None
    );
    let transactions = Arc::new(SnapshotManager::new());
    let writer = transactions.begin();
    let mut entity = UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![]);
    entity.xmin = writer;
    store.insert("rows", entity).expect("row");
    store.persist().expect("physical version fixture");
    let before = context(&transactions);
    compare(&store, Some(&before), 0);
    transactions.commit(writer);
    compare(&store, Some(&before), 0);
    compare(&store, Some(&context(&transactions)), 1);
}

#[test]
fn paged_count_legacy_positional_moderation_uses_bound_schema() {
    let projection = VisibilityProjection {
        schema: Some(Arc::new(vec![
            "payload".into(),
            "__moderation_status".into(),
        ])),
    };
    for (marker, hidden) in [("pending", 1), ("rejected", 1), ("", 0)] {
        // Legacy positional records have neither per-row names nor a schema.
        let entity = UnifiedEntity::table_row(
            EntityId::new(1),
            "rows",
            1,
            vec![Value::text("payload"), Value::text(marker)],
        );
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        assert_eq!(
            projection.project(&entity, &mut key).expect("project"),
            Some(17)
        );
        assert_eq!(key[16], hidden);
    }
    assert_ne!(
        identity(projection.schema.as_deref().map(Vec::as_slice)),
        identity(None)
    );
}
