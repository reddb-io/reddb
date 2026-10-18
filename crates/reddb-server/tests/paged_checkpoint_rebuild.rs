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
