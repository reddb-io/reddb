use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct IdentityProjection {
    visits: Arc<AtomicUsize>,
    fail_at: usize,
}

impl SecondaryIndexProjector for IdentityProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let visit = self.visits.fetch_add(1, Ordering::SeqCst);
        if visit == self.fail_at {
            return Err(StoreError::Io(std::io::Error::other(
                "injected projection failure",
            )));
        }
        // Repeated logical values must retain distinct physical/MVCC versions.
        key[..8].copy_from_slice(&(entity.id.raw() % 3).to_be_bytes());
        Ok(Some(8))
    }
}

fn config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(32),
        resident_memory_bytes_max: Some(2 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    }
}

fn identity() -> SecondaryIndexIdentity {
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: [42; 32],
    }
}

fn register(store: &UnifiedStore, visits: &Arc<AtomicUsize>, fail_at: usize) {
    store
        .register_secondary_index(
            "rows",
            "by_remainder",
            identity(),
            Arc::new(IdentityProjection {
                visits: Arc::clone(visits),
                fail_at,
            }),
        )
        .expect("register secondary");
}

fn insert(store: &UnifiedStore, id: u64) {
    let entity = UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]);
    store.insert("rows", entity).expect("insert");
}

fn postings(tree: &SecondaryIndexRead) -> Vec<(u64, u64)> {
    let mut entries = Vec::new();
    for key in 0..3u64 {
        let mut cursor = tree.candidates(&key.to_be_bytes()).expect("cursor");
        while let Some(id) = cursor.next_id().expect("read") {
            entries.push((key, id.raw()));
        }
    }
    entries
}

#[test]
fn cold_versioned_update_projects_only_changed_rows_and_recovers_count() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("update.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("rows").unwrap();
    register(&store, &visits, usize::MAX);
    store.register_count_index("rows").unwrap();
    for id in 1..=256 {
        insert(&store, id);
    }
    store.persist().unwrap();
    drop(store);

    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    register(&store, &visits, usize::MAX);
    store.register_count_index("rows").unwrap();
    let mut old = store
        .try_get("rows", EntityId::new(3), 65536)
        .unwrap()
        .unwrap();
    old.set_xmax(7);
    let mut new = UnifiedEntity::table_row(EntityId::new(257), "rows", 3, vec![]);
    new.set_xmin(7);
    store
        .install_versioned_table_row_update("rows", old, new, None)
        .unwrap();
    let before = visits.load(Ordering::SeqCst);
    store.persist().unwrap();
    assert_eq!(visits.load(Ordering::SeqCst) - before, 3);
    drop(store);

    let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
    assert_eq!(
        reopened.try_count_from_index("rows", None).unwrap(),
        Some(256)
    );
    let index = reopened
        .secondary_index("rows", "by_remainder", &identity())
        .unwrap();
    let entries = postings(&index);
    assert_eq!(entries.len(), 257);
    assert!(entries.contains(&(0, 3)) && entries.contains(&(2, 257)));
    assert_eq!(
        reopened
            .try_get("rows", EntityId::new(3), 65536)
            .unwrap()
            .unwrap()
            .xmax,
        7
    );
    drop(index);
    register(&reopened, &visits, usize::MAX);
    reopened.register_count_index("rows").unwrap();
    reopened.delete_batch("rows", &[EntityId::new(3)]).unwrap();
    let before = visits.load(Ordering::SeqCst);
    reopened.persist().unwrap();
    assert_eq!(visits.load(Ordering::SeqCst) - before, 1);
    drop(reopened);

    let collected = UnifiedStore::open_with_config(&path, config()).unwrap();
    assert_eq!(
        collected.try_count_from_index("rows", None).unwrap(),
        Some(256)
    );
    let index = collected
        .secondary_index("rows", "by_remainder", &identity())
        .unwrap();
    let entries = postings(&index);
    assert_eq!(entries.len(), 256);
    assert!(!entries.contains(&(0, 3)) && entries.contains(&(2, 257)));
    assert!(collected
        .try_get("rows", EntityId::new(3), 65536)
        .unwrap()
        .is_none());
}

#[test]
fn cold_logged_removal_batch_projects_only_the_removed_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reclaim_batch.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("rows").unwrap();
    register(&store, &visits, usize::MAX);
    store.register_count_index("rows").unwrap();
    for id in 1..=1024 {
        insert(&store, id);
    }
    store.persist().unwrap();

    let removed: Vec<_> = (1..=256).map(EntityId::new).collect();
    assert_eq!(
        store
            .delete_batch_between_walks("rows", &removed)
            .unwrap()
            .len(),
        256
    );
    let before = visits.load(Ordering::SeqCst);
    store.persist().unwrap();
    assert_eq!(visits.load(Ordering::SeqCst) - before, 256);
    drop(store);

    let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
    register(&reopened, &visits, usize::MAX);
    reopened.register_count_index("rows").unwrap();
    assert_eq!(
        reopened.try_count_from_index("rows", None).unwrap(),
        Some(768)
    );
    let index = reopened
        .secondary_index("rows", "by_remainder", &identity())
        .unwrap();
    let expected: std::collections::BTreeSet<_> = (257..=1024).map(|id| (id % 3, id)).collect();
    assert_eq!(
        postings(&index)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
}

#[test]
fn cold_logged_removal_recovers_before_and_after_catalog_publication() {
    const CHILD: &str = "REDDB_TEST_COLD_LOGGED_REMOVAL";
    const HOOK: &str = "REDDB_TEST_COLD_LOGGED_REMOVAL_HOOK";
    let visits = Arc::new(AtomicUsize::new(0));
    if let Some(path) = std::env::var_os(CHILD) {
        let store = UnifiedStore::open_with_config(PathBuf::from(path), config()).unwrap();
        register(&store, &visits, usize::MAX);
        store.register_count_index("rows").unwrap();
        store.delete_batch("rows", &[EntityId::new(3)]).unwrap();
        store
            .try_set_config_tree("app.color", &crate::serde_json::json!("blue"))
            .unwrap();
        std::env::set_var(std::env::var(HOOK).unwrap(), "1");
        store.persist().unwrap();
        panic!("checkpoint did not reach the selected publication phase");
    }
    for (hook, code) in [
        ("REDDB_TEST_EXIT_AFTER_SECONDARY_INDEX_BUILD", 196),
        ("REDDB_TEST_EXIT_AFTER_UNDO_CATALOG_PUBLICATION", 195),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("removal.rdb");
        let store = UnifiedStore::open_with_config(&path, config()).unwrap();
        store.create_collection("rows").unwrap();
        register(&store, &visits, usize::MAX);
        store.register_count_index("rows").unwrap();
        for id in 1..=64 {
            insert(&store, id);
        }
        store
            .try_set_config_tree("app.color", &crate::serde_json::json!("red"))
            .unwrap();
        store.persist().unwrap();
        drop(store);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::unified::store::secondary_index::tests::cold_logged_removal_recovers_before_and_after_catalog_publication"])
            .env(CHILD, &path)
            .env(HOOK, hook)
            .status().unwrap();
        assert_eq!(status.code(), Some(code), "{hook}");
        let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(
            reopened.try_get_config("app.color").unwrap(),
            Some(reddb_types::Value::text("blue")),
            "{hook}"
        );
        assert!(reopened
            .try_get("rows", EntityId::new(3), 65536)
            .unwrap()
            .is_none());
        register(&reopened, &visits, usize::MAX);
        reopened.register_count_index("rows").unwrap();
        reopened.persist().unwrap();
        assert_eq!(
            reopened.try_count_from_index("rows", None).unwrap(),
            Some(63)
        );
        let index = reopened
            .secondary_index("rows", "by_remainder", &identity())
            .unwrap();
        let entries = postings(&index);
        assert_eq!(entries.len(), 63);
        assert!(!entries.contains(&(0, 3)));
    }
}

#[test]
fn sparse_keyed_checkpoint_reuses_trees_and_replaces_physical_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keyed.rdb");
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    let visits = Arc::new(AtomicUsize::new(0));
    let register_index = |store: &UnifiedStore| {
        store
            .register_secondary_index(
                "red.config",
                "by_remainder",
                identity(),
                Arc::new(IdentityProjection {
                    visits: Arc::clone(&visits),
                    fail_at: usize::MAX,
                }),
            )
            .unwrap();
    };
    for n in 0..128 {
        store
            .try_set_config_tree(&format!("app.key{n}"), &crate::serde_json::json!(n))
            .unwrap();
    }
    let key = "app.color";
    store
        .try_set_config_tree(key, &crate::serde_json::json!("red"))
        .unwrap();
    register_index(&store);
    store.persist().unwrap();
    drop(store);

    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    register_index(&store);
    let old = store.try_get_config_entity(key).unwrap().unwrap().id;
    let primary = Arc::downgrade(&store.btree_indices.read()["red.config"]);
    let candidates = Arc::downgrade(&store.key_indices.read()["red.config"].tree);
    store
        .try_set_config_tree(key, &crate::serde_json::json!("blue"))
        .unwrap();
    let new = store.try_get_config_entity(key).unwrap().unwrap().id;
    assert_ne!(old, new);
    let before = visits.load(Ordering::SeqCst);
    store.persist().unwrap();
    assert_eq!(visits.load(Ordering::SeqCst) - before, 2);
    assert!(primary.ptr_eq(&Arc::downgrade(&store.btree_indices.read()["red.config"])));
    assert!(candidates.ptr_eq(&Arc::downgrade(
        &store.key_indices.read()["red.config"].tree
    )));
    drop(store);

    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    assert_eq!(
        store.try_get_config(key).unwrap(),
        Some(reddb_types::Value::text("blue"))
    );
    let keys = store.key_indices.read();
    let tree = &keys["red.config"].tree;
    let fingerprint = reddb_file::native_key_index::fingerprint(key);
    for (id, present) in [(old, false), (new, true)] {
        let candidate = reddb_file::native_key_index::encode_candidate(&fingerprint, id.raw());
        assert_eq!(tree.get(&candidate).unwrap().is_some(), present);
    }
    let pinned = Arc::clone(tree);
    drop(keys);
    register_index(&store);
    store
        .try_set_config_tree(key, &crate::serde_json::json!("green"))
        .unwrap();
    let before = visits.load(Ordering::SeqCst);
    store.persist().unwrap();
    assert_eq!(visits.load(Ordering::SeqCst) - before, 129);
    let blue = reddb_file::native_key_index::encode_candidate(&fingerprint, new.raw());
    assert!(pinned.get(&blue).unwrap().is_some());
    assert!(store.key_indices.read()["red.config"]
        .tree
        .get(&blue)
        .unwrap()
        .is_none());
}

#[test]
fn secondary_sparse_append_projects_only_new_rows_and_preserves_count() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("append.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("rows").unwrap();
    for id in 1..=2048 {
        insert(&store, id);
    }
    register(&store, &visits, usize::MAX);
    store.register_count_index("rows").unwrap();
    store.persist().unwrap();
    drop(store);
    for id in 2049..=2051 {
        let store = UnifiedStore::open_with_config(&path, config()).unwrap();
        register(&store, &visits, usize::MAX);
        store.register_count_index("rows").unwrap();
        insert(&store, id);
        let before = visits.load(Ordering::SeqCst);
        store.persist().unwrap();
        assert_eq!(visits.load(Ordering::SeqCst) - before, 1);
        assert_eq!(store.try_count_from_index("rows", None).unwrap(), Some(id));
        let index = store
            .secondary_index("rows", "by_remainder", &identity())
            .unwrap();
        let mut expected: Vec<_> = (1..=id).map(|id| (id % 3, id)).collect();
        expected.sort_unstable();
        assert_eq!(postings(&index), expected);
    }
}

#[test]
fn secondary_sparse_projection_failure_keeps_published_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("append-fail.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("rows").unwrap();
    for id in 1..=20 {
        insert(&store, id);
    }
    register(&store, &visits, usize::MAX);
    store.persist().unwrap();
    insert(&store, 21);
    register(&store, &visits, 20);
    assert!(store.persist().is_err());
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .unwrap()
        )
        .len(),
        20
    );
    drop(store);
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    register(&store, &visits, usize::MAX);
    store.persist().unwrap();
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .unwrap()
        )
        .len(),
        21
    );
}

#[test]
fn secondary_cold_reopen_keeps_postings_without_backfill() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("secondary.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    for id in 1..=513 {
        insert(&store, id);
    }
    register(&store, &visits, usize::MAX);
    store.persist().expect("publish");
    let index = store
        .secondary_index("rows", "by_remainder", &identity())
        .expect("index");
    let expected = postings(&index);
    assert_eq!(expected.len(), 513);
    assert_eq!(visits.load(Ordering::SeqCst), 513);
    let root = index.root_page_id();
    drop(index);
    drop(store);
    for _ in 0..3 {
        let store = UnifiedStore::open_with_config(&path, config()).expect("reopen");
        // A projector that always fails proves cold open and a no-op checkpoint
        // reuse the published roots, rather than rebuilding source payloads.
        register(&store, &visits, 513);
        let index = store
            .secondary_index("rows", "by_remainder", &identity())
            .expect("resumed");
        assert_eq!(index.root_page_id(), root);
        assert_eq!(postings(&index), expected);
        let mut different = identity();
        different.definition[0] ^= 1;
        assert!(store
            .secondary_index("rows", "by_remainder", &different)
            .is_none());
        store.persist().expect("no-op checkpoint");
        assert_eq!(visits.load(Ordering::SeqCst), 513);
    }
}

#[test]
fn secondary_generation_replacement_preserves_pinned_reader_and_invalidates_without_builder() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("generations.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    insert(&store, 1);
    register(&store, &visits, usize::MAX);
    store.persist().expect("first generation");
    let old = store
        .secondary_index("rows", "by_remainder", &identity())
        .expect("old");
    for id in 2..=8 {
        insert(&store, id);
        store.persist().expect("new generation");
        assert_eq!(postings(&old), vec![(1, 1)], "pinned pages were recycled");
        let current = store
            .secondary_index("rows", "by_remainder", &identity())
            .expect("new");
        assert_eq!(postings(&current).len(), id as usize);
    }
    drop(old);
    drop(store);
    let store = UnifiedStore::open_with_config(&path, config()).expect("reopen");
    insert(&store, 9);
    store.persist().expect("changed data without projector");
    assert!(
        store
            .secondary_index("rows", "by_remainder", &identity())
            .is_none(),
        "old generation cannot be trusted for new data"
    );
    register(&store, &visits, usize::MAX);
    store.persist().expect("prepare missing index");
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .expect("rebuilt")
        )
        .len(),
        9
    );
}

#[test]
fn secondary_failed_build_never_publishes_partial_postings() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("failed.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    for id in 1..=20 {
        insert(&store, id);
    }
    register(&store, &visits, usize::MAX);
    store.persist().expect("first checkpoint");
    // Keep this failure in the immutable replacement path: an old reader
    // pins the generation while the new projector fails partway through it.
    let pinned = store
        .secondary_index("rows", "by_remainder", &identity())
        .unwrap();
    insert(&store, 21);
    register(&store, &visits, 25);
    assert!(store.persist().is_err(), "injected failure must surface");
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .expect("old published index")
        )
        .len(),
        20
    );
    drop(pinned);
    drop(store);
    let store =
        UnifiedStore::open_with_config(&path, config()).expect("ownership and WAL recovery");
    register(&store, &visits, usize::MAX);
    store.persist().expect("replacement after recovery");
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .expect("complete index")
        )
        .len(),
        21
    );
}

#[test]
fn secondary_empty_reopen_drop_and_definition_change_are_not_stale_hits() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("empty.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    register(&store, &visits, usize::MAX);
    store.persist().expect("empty checkpoint");
    drop(store);
    let store = UnifiedStore::open_with_config(&path, config()).expect("empty reopen");
    assert!(postings(
        &store
            .secondary_index("rows", "by_remainder", &identity())
            .expect("prepared empty index")
    )
    .is_empty());
    insert(&store, 1);
    register(&store, &visits, usize::MAX);
    store.persist().expect("populated");
    let old = store
        .secondary_index("rows", "by_remainder", &identity())
        .expect("old definition");
    let mut replacement = identity();
    replacement.definition[1] ^= 1;
    store
        .register_secondary_index(
            "rows",
            "by_remainder",
            replacement.clone(),
            Arc::new(IdentityProjection {
                visits: Arc::clone(&visits),
                fail_at: usize::MAX,
            }),
        )
        .expect("redefinition");
    assert!(store
        .secondary_index("rows", "by_remainder", &replacement)
        .is_none());
    store.persist().expect("publish new definition");
    let indexed_version = store.pager().unwrap().header().unwrap().version;
    assert!(indexed_version >= reddb_file::file_format::PAGE_SECONDARY_INDEX_FILE_VERSION);
    assert!(store
        .secondary_index("rows", "by_remainder", &identity())
        .is_none());
    assert_eq!(postings(&old), vec![(1, 1)]);
    store.unregister_secondary_index("rows", "by_remainder");
    store.persist().expect("publish drop");
    assert_eq!(postings(&old), vec![(1, 1)]);
    drop(old);
    drop(store);
    let store = UnifiedStore::open_with_config(&path, config()).expect("reopen dropped");
    assert!(store
        .secondary_index("rows", "by_remainder", &replacement)
        .is_none());
    assert_eq!(
        store
            .pager()
            .expect("pager")
            .header()
            .expect("header")
            .version,
        indexed_version
    );
}

#[test]
fn secondary_crash_before_and_after_catalog_recovers_complete_generations() {
    const CHILD_PATH: &str = "REDDB_SECONDARY_CRASH_TEST_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let store =
            UnifiedStore::open_with_config(PathBuf::from(path), config()).expect("child open");
        let visits = Arc::new(AtomicUsize::new(0));
        register(&store, &visits, usize::MAX);
        insert(&store, 21);
        store.persist().expect("child reaches crash hook");
        panic!("crash hook was not reached");
    }
    for (hook, code) in [
        ("REDDB_TEST_EXIT_AFTER_SECONDARY_INDEX_BUILD", 196),
        ("REDDB_TEST_EXIT_AFTER_UNDO_CATALOG_PUBLICATION", 195),
    ] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("crash.rdb");
        let visits = Arc::new(AtomicUsize::new(0));
        let store = UnifiedStore::open_with_config(&path, config()).expect("open");
        store.create_collection("rows").expect("collection");
        for id in 1..=20 {
            insert(&store, id);
        }
        register(&store, &visits, usize::MAX);
        store.persist().expect("baseline checkpoint");
        drop(store);
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "storage::unified::store::secondary_index::tests::secondary_crash_before_and_after_catalog_recovers_complete_generations", "--nocapture"])
            .env(CHILD_PATH, &path).env(hook, "1").output().expect("child process");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{hook}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let store = UnifiedStore::open_with_config(&path, config()).expect("recover");
        if code == 195 {
            assert_eq!(
                postings(
                    &store
                        .secondary_index("rows", "by_remainder", &identity())
                        .expect("published crash generation")
                )
                .len(),
                21
            );
        }
        register(&store, &visits, usize::MAX);
        store.persist().expect("complete recovered generation");
        let mut expected: Vec<_> = (1..=21).map(|id| (id % 3, id)).collect();
        expected.sort_unstable();
        assert_eq!(
            postings(
                &store
                    .secondary_index("rows", "by_remainder", &identity())
                    .expect("recovered index")
            ),
            expected
        );
        drop(store);
        let store = UnifiedStore::open_with_config(&path, config()).expect("idempotent recovery");
        assert_eq!(
            postings(
                &store
                    .secondary_index("rows", "by_remainder", &identity())
                    .expect("reopened index")
            ),
            expected
        );
    }
}

#[test]
fn secondary_cursor_pins_pages_and_corruption_is_not_an_empty_lookup() {
    use crate::storage::engine::{Page, PageType};
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("cursor.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    insert(&store, 1);
    register(&store, &visits, usize::MAX);
    store.persist().expect("initial");
    let old = store
        .secondary_index("rows", "by_remainder", &identity())
        .expect("index");
    let mut cursor = old.candidates(&1u64.to_be_bytes()).expect("pinned cursor");
    drop(old);
    for id in 2..=5 {
        insert(&store, id);
        store.persist().expect("replace");
    }
    assert_eq!(
        cursor.next_id().expect("old reader"),
        Some(EntityId::new(1))
    );
    assert_eq!(cursor.next_id().expect("old reader end"), None);
    drop(cursor);

    let current = store
        .secondary_index("rows", "by_remainder", &identity())
        .expect("current");
    assert!(current
        .candidates(&vec![0; SECONDARY_KEY_BYTES_MAX + 1])
        .is_err());
    let root = current.root_page_id();
    let pager = store.pager().expect("pager");
    let original = pager.read_page(root).expect("original leaf");
    pager
        .write_page(root, Page::new(PageType::Overflow, root))
        .expect("inject wrong page kind");
    let failed = match current.candidates(&1u64.to_be_bytes()) {
        Err(_) => true,
        Ok(mut cursor) => cursor.next_id().is_err(),
    };
    pager.write_page(root, original).expect("restore fixture");
    assert!(
        failed,
        "unreadable index must not produce a successful empty lookup"
    );
}

#[test]
fn secondary_constant_live_data_churn_reuses_pages() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("churn.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    register(&store, &visits, usize::MAX);
    let mut warmed_pages = 0;
    for cycle in 0..120 {
        for id in 1..=10 {
            if cycle > 0 {
                assert!(store
                    .delete("rows", EntityId::new((cycle - 1) * 10 + id))
                    .expect("delete prior row"));
            }
            insert(&store, cycle * 10 + id);
        }
        store.persist().expect("churn checkpoint");
        assert_eq!(
            postings(
                &store
                    .secondary_index("rows", "by_remainder", &identity())
                    .expect("current generation")
            )
            .len(),
            10
        );
        let pages = store
            .pager()
            .expect("pager")
            .page_count()
            .expect("page count");
        if cycle == 19 {
            warmed_pages = pages;
        }
        if cycle >= 20 {
            assert!(
                pages <= warmed_pages + 4,
                "constant live data grew: {warmed_pages} -> {pages} pages at cycle {cycle}"
            );
        }
    }
}

#[test]
fn secondary_rebuild_larger_than_one_run_keeps_every_posting_in_key_order() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("runs.rdb");
    let visits = Arc::new(AtomicUsize::new(0));
    let store = UnifiedStore::open_with_config(&path, config()).expect("open");
    store.create_collection("rows").expect("collection");
    // The fixture's memory limit holds about twelve thousand postings a run:
    // the first is loaded sorted, the rest land among it.
    let rows = 30_000;
    for id in 1..=rows {
        insert(&store, id);
    }
    register(&store, &visits, usize::MAX);
    store.persist().expect("checkpoint");
    let expected: Vec<(u64, u64)> = (0..3)
        .flat_map(|key| {
            (1..=rows)
                .filter(move |id| id % 3 == key)
                .map(move |id| (key, id))
        })
        .collect();
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .expect("published index")
        ),
        expected
    );
    drop(store);
    let store = UnifiedStore::open_with_config(&path, config()).expect("reopen");
    register(&store, &visits, usize::MAX);
    assert_eq!(
        postings(
            &store
                .secondary_index("rows", "by_remainder", &identity())
                .expect("reopened index")
        ),
        expected
    );
}
