//! A failed checkpoint must leave its published tree generation readable.
use reddb_server::storage::engine::{BTree, HEADER_SIZE, PAGE_SIZE};
use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
use reddb_types::Value;
use std::{path::Path, sync::Arc};

fn open(path: &Path) -> UnifiedStore {
    UnifiedStore::open_with_config(path, UnifiedStoreConfig::default().with_page_cache_slots(4))
        .unwrap()
}

fn payload(id: u64) -> String {
    let mut state = id + 1;
    (0..1800)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            char::from(b'!' + ((state >> 24) % 90) as u8)
        })
        .collect()
}

fn seed(path: &Path) {
    let store = open(path);
    store.create_collection("rows").unwrap();
    for id in 1..=400 {
        store
            .insert(
                "rows",
                UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text(payload(id))],
                ),
            )
            .unwrap();
    }
    store.persist().unwrap();
}

fn root(store: &UnifiedStore) -> u32 {
    let page = store.pager().unwrap().read_manifest_page().unwrap();
    let data = &page.as_bytes()[HEADER_SIZE..];
    let header = reddb_file::decode_native_paged_metadata_header(data)
        .unwrap()
        .unwrap();
    assert_eq!(header.collection_count, 1);
    let mut position = reddb_file::METADATA_HEADER_BYTES;
    let collection = reddb_file::decode_native_paged_collection_root(data, &mut position).unwrap();
    assert_eq!(collection.collection, "rows");
    collection.root_page
}

fn insert_unpublished_row(store: &UnifiedStore) {
    store
        .get_collection("rows")
        .unwrap()
        .insert(UnifiedEntity::table_row(
            EntityId::new(401),
            "rows",
            401,
            vec![Value::text(payload(401))],
        ))
        .unwrap();
}

#[test]
fn unchanged_checkpoint_reuses_the_published_tree_without_growing_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    seed(&path);
    let store = open(&path);
    let old_root = root(&store);
    let old_length = std::fs::metadata(&path).unwrap().len();
    for _ in 0..3 {
        store.persist().unwrap();
        assert_eq!(root(&store), old_root);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), old_length);
    }
}

#[test]
fn repeated_changed_checkpoints_reuse_retired_pages() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    seed(&path);
    let store = open(&path);
    let manager = store.get_collection("rows").expect("rows");
    let initial_bytes = std::fs::metadata(&path).expect("database file").len();
    for revision in 1..=24 {
        let mut entity = manager.get(EntityId::new(1)).expect("seeded row");
        entity.updated_at += 1;
        manager.update(entity).expect("same-sized update");
        store.persist().expect("changed checkpoint");
        let bytes = std::fs::metadata(&path).expect("database file").len();
        assert!(
            bytes <= initial_bytes * 4,
            "checkpoint {revision} grew a fixed dataset from {initial_bytes} to {bytes} bytes"
        );
    }
    drop(manager);
    drop(store);
    let recovered = open(&path);
    assert_eq!(recovered.get_collection("rows").expect("rows").count(), 400);
}

#[test]
fn retired_generations_are_reused_across_restarts() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    seed(&path);
    let initial_bytes = std::fs::metadata(&path).expect("database file").len();
    for _ in 0..12 {
        let store = open(&path);
        let manager = store.get_collection("rows").expect("rows");
        let mut entity = manager.get(EntityId::new(1)).expect("seeded row");
        entity.updated_at += 1;
        manager.update(entity).expect("same-sized update");
        store.persist().expect("checkpoint before restart");
        assert!(std::fs::metadata(&path).expect("database file").len() <= initial_bytes * 4);
        assert_eq!(manager.count(), 400);
    }
}

#[test]
fn checkpoint_retains_the_previously_published_tree_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    seed(&path);
    let store = open(&path);
    let pager = Arc::clone(store.pager().unwrap());
    let old_root = root(&store);
    let old_bytes = pager.read_page(old_root).unwrap().as_bytes().to_vec();
    insert_unpublished_row(&store);
    store.persist().unwrap();
    assert_ne!(
        root(&store),
        old_root,
        "checkpoint must publish a replacement tree"
    );
    assert!(
        pager.read_page(old_root).unwrap().as_bytes().as_slice() == old_bytes.as_slice(),
        "previous checkpoint root was overwritten"
    );
    let old = BTree::with_root(pager, old_root);
    let mut cursor = old.cursor_first().unwrap();
    let mut count = 0;
    while cursor.next().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 400);
}

#[test]
fn physical_metadata_fingerprint_changes_after_an_update_without_a_row_count_change() {
    use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    runtime
        .execute_query("CREATE TABLE checkpoint_rows (value INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO checkpoint_rows (value) VALUES (1)")
        .expect("row");
    runtime.checkpoint().expect("initial checkpoint");
    let physical_root = || {
        runtime
            .db()
            .physical_metadata()
            .expect("metadata")
            .superblock
            .collection_roots["checkpoint_rows"]
    };
    let before = physical_root();
    runtime.checkpoint().expect("reuse fingerprint");
    assert_eq!(physical_root(), before);

    let manager = runtime
        .db()
        .store()
        .get_collection("checkpoint_rows")
        .expect("rows");
    let mut entity = manager.query_all(|_| true).remove(0);
    entity.updated_at += 1;
    manager.update(entity).expect("same-sized mutation");
    runtime.checkpoint().expect("invalidate fingerprint");
    assert_ne!(
        physical_root(),
        before,
        "a cached root must not hide a same-sized update"
    );
    let after = physical_root();
    runtime.checkpoint().expect("reuse updated fingerprint");
    assert_eq!(physical_root(), after);
}

#[test]
#[cfg(target_os = "linux")]
fn failed_checkpoint_preserves_all_previously_committed_rows() {
    const CHILD: &str = "REDDB_TEST_CHECKPOINT_FSIZE_PATH";
    if let Some(path) = std::env::var_os(CHILD) {
        let path = Path::new(&path);
        let store = open(path);
        insert_unpublished_row(&store);
        let limit = (std::fs::metadata(path).unwrap().len() / 2).max((64 * PAGE_SIZE) as u64);
        // Apply a file-size bound only in the isolated child. Ignoring SIGXFSZ
        // turns a partial checkpoint's next growth into a real EFBIG I/O error.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            assert_eq!(
                libc::setrlimit(
                    libc::RLIMIT_FSIZE,
                    &libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    }
                ),
                0
            );
        }
        let failed = store.persist().is_err();
        // Do not let destructors or another checkpoint repair this crash image.
        std::process::exit(if failed { 0 } else { 91 });
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    seed(&path);
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "failed_checkpoint_preserves_all_previously_committed_rows",
            "--nocapture",
        ])
        .env(CHILD, &path)
        .status()
        .unwrap();
    assert!(
        result.success(),
        "child must hit the checkpoint I/O failure: {result}"
    );
    let recovered = open(&path);
    assert_eq!(recovered.get_collection("rows").unwrap().count(), 400);
    for id in 1..=400 {
        let entity = recovered
            .get_collection("rows")
            .unwrap()
            .get(EntityId::new(id))
            .unwrap();
        assert_eq!(
            entity.data.as_row().unwrap().columns,
            vec![Value::text(payload(id))]
        );
    }
}

#[test]
fn crash_during_freelist_reuse_preserves_allocator() {
    const CHILD: &str = "REDDB_TEST_RECLAIM_ALLOCATOR_PATH";
    let open_pager = |path: &Path| {
        reddb_server::storage::engine::Pager::open(
            path,
            reddb_server::storage::engine::PagerConfig {
                cache_size: 1,
                ..Default::default()
            },
        )
        .expect("pager")
    };
    if let Some(path) = std::env::var_os(CHILD) {
        let pager = open_pager(Path::new(&path));
        for _ in 0..2048 {
            pager
                .allocate_page(reddb_server::storage::engine::PageType::BTreeLeaf)
                .expect("reuse");
        }
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let pager = open_pager(&path);
    let mut pages = Vec::new();
    for _ in 0..32 {
        pages.push(
            pager
                .allocate_page(reddb_server::storage::engine::PageType::BTreeLeaf)
                .expect("allocate")
                .page_id(),
        );
    }
    pager.sync().expect("seed");
    for id in pages {
        pager.free_page(id).expect("free");
    }
    pager.sync().expect("publish free list");
    drop(pager);
    assert!(
        std::process::Command::new(std::env::current_exe().expect("self"))
            .args(["--exact", "crash_during_freelist_reuse_preserves_allocator"])
            .env(CHILD, &path)
            .status()
            .expect("crash child")
            .success()
    );
    let pager = open_pager(&path);
    pager
        .allocate_page(reddb_server::storage::engine::PageType::BTreeLeaf)
        .expect("a crash during page reuse must not destroy the allocator");
}

#[test]
fn document_indexes_survive_reused_checkpoints_and_uncheckpointed_wal_recovery() {
    use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult, StorageDeployPreset};

    const CHILD: &str = "REDDB_TEST_INDEX_RECOVERY_PATH";
    let runtime = |path: &Path| {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_storage_profile(StorageDeployPreset::Serverless.selection())
                .expect("paged profile"),
        )
        .expect("persistent runtime")
    };
    let names = |result: RuntimeQueryResult| {
        result
            .result
            .records
            .iter()
            .map(|row| match row.get("name") {
                Some(Value::Text(name)) => name.to_string(),
                other => panic!("expected document name, got {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    if let Some(path) = std::env::var_os(CHILD) {
        let rt = runtime(Path::new(&path));
        for revision in 1..=12 {
            rt.execute_query(&format!(
                "UPDATE indexed_docs SET score = {} WHERE name = 'bob'",
                100 + revision
            ))
            .expect("indexed update");
            rt.checkpoint().expect("changed checkpoint");
            assert_eq!(
                names(
                    rt.execute_query(
                        "SELECT name FROM indexed_docs WHERE score > 40 ORDER BY score ASC"
                    )
                    .expect("ordered range during page reuse")
                ),
                vec!["carol", "bob"]
            );
        }
        rt.execute_query("UPDATE indexed_docs SET score = 200 WHERE name = 'alice'")
            .expect("acknowledged write after the last checkpoint");
        // Skip all destructors: recovery must use the durable checkpoint + WAL,
        // rather than an orderly shutdown rebuilding/persisting the indexes.
        std::process::exit(0);
    }

    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("indexes.rdb");
    {
        let rt = runtime(&path);
        rt.execute_query("CREATE DOCUMENT indexed_docs")
            .expect("document collection");
        for (name, score) in [("alice", 30), ("bob", 10), ("carol", 50), ("dave", 20)] {
            rt.execute_query(&format!(
                "INSERT INTO indexed_docs DOCUMENT VALUES ({{\"name\":\"{name}\",\"score\":{score}}})"
            ))
            .expect("seed document");
        }
        rt.execute_query("CREATE INDEX idx_score ON indexed_docs (score) USING BTREE")
            .expect("ordered index");
        rt.execute_query("CREATE INDEX idx_name ON indexed_docs (name) USING HASH")
            .expect("hash index");
        rt.checkpoint().expect("seed checkpoint");
    }
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "document_indexes_survive_reused_checkpoints_and_uncheckpointed_wal_recovery",
        ])
        .env(CHILD, &path)
        .status()
        .expect("crash child");
    assert!(status.success(), "checkpoint child failed: {status}");

    let rt = runtime(&path);
    assert!(rt
        .index_store_ref()
        .sorted
        .has_index("indexed_docs", "score"));
    assert_eq!(
        names(
            rt.execute_query("SELECT name FROM indexed_docs WHERE name = 'alice' AND score = 200")
                .expect("hash lookup after WAL recovery")
        ),
        vec!["alice"]
    );
    assert_eq!(
        names(
            rt.execute_query("SELECT name FROM indexed_docs WHERE score > 40 ORDER BY score ASC")
                .expect("ordered range after WAL recovery")
        ),
        vec!["carol", "bob", "alice"]
    );
    assert_eq!(
        names(
            rt.execute_query("SELECT name FROM indexed_docs ORDER BY score DESC LIMIT 2")
                .expect("top-N after WAL recovery")
        ),
        vec!["alice", "bob"]
    );
}
