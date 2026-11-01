use reddb_engine::storage::btree::{BTree, BTreeError};
use reddb_engine::storage::collection_read::{
    CollectionCursor, CollectionRead, FrozenOverlay, MergedCollection, PagedCollection,
};
use reddb_engine::storage::entity::{EntityId, UnifiedEntity};
use reddb_engine::storage::error::StoreError;
use reddb_engine::storage::metadata_codec::serialize_entity_record;
use reddb_engine::storage::page::{Page, PageType};
use reddb_engine::storage::page_store::PageStore;
use reddb_engine::storage::pager::PagerError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Pages {
    next: AtomicU32,
    pages: Mutex<HashMap<u32, Page>>,
    overflow_reads: AtomicUsize,
    reads: AtomicUsize,
}

impl PageStore for Pages {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let page = self.pages.lock().expect("fixture lock")[&id].clone();
        if page.page_type().expect("fixture page type") == PageType::Overflow {
            self.overflow_reads.fetch_add(1, Ordering::Relaxed);
        }
        Ok(page)
    }

    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        self.pages.lock().expect("fixture lock").insert(id, page);
        Ok(())
    }

    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let page = Page::new(kind, id);
        self.write_page(id, page.clone())?;
        Ok(page)
    }

    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.pages.lock().expect("fixture lock").remove(&id);
        Ok(())
    }

    fn page_count(&self) -> Result<u32, PagerError> {
        Ok(self.next.load(Ordering::Relaxed) + 1)
    }
}

fn noise(bytes: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_u32;
    (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn medium_values_pack_into_native_leaves_without_dedicated_overflow_pages() {
    const ROWS: u64 = 256;
    for length in [1025, 2048, 3072, 4096] {
        let pages = Arc::new(Pages::default());
        let tree = BTree::new(Arc::clone(&pages));
        let value = noise(length);
        for id in 0..ROWS {
            tree.insert(&id.to_be_bytes(), &value)
                .expect("medium value");
        }
        let images = pages.pages.lock().expect("fixture lock");
        let overflow_pages = images
            .values()
            .filter(|page| page.page_type().expect("page type") == PageType::Overflow)
            .count();
        let physical_bytes = images.len() * reddb_engine::storage::page::PAGE_SIZE;
        let payload_bytes = usize::try_from(ROWS).expect("fixture row count") * length;
        eprintln!("medium values: length={length} physical_bytes={physical_bytes} payload_bytes={payload_bytes} overflow_pages={overflow_pages}");
        assert_eq!(
            overflow_pages, 0,
            "medium values must share the 16-KiB leaves"
        );
        assert!(
            physical_bytes <= payload_bytes * 3 + 4 * reddb_engine::storage::page::PAGE_SIZE,
            "splits and interior pages must not turn medium records into page-per-row storage"
        );
        drop(images);
        let mut cursor = tree.cursor_first().expect("readback cursor");
        for id in 0..ROWS {
            let (key, actual) = cursor.next().expect("readback").expect("row");
            assert_eq!(key, id.to_be_bytes());
            assert_eq!(actual, value);
        }
        assert!(cursor.next().expect("end").is_none());
    }
}

#[test]
fn medium_compressible_values_keep_the_existing_compression_policy() {
    use reddb_engine::storage::btree::value_layout;
    let pages = Pages::default();
    for length in [1025, 2048, 3072, 4096] {
        let value = vec![b'x'; length];
        let encoded = value_layout::encode(&pages, &value).expect("compressed medium value");
        assert!(
            matches!(
                reddb_file::decode_btree_value_cell(&encoded).expect("cell framing"),
                reddb_file::BTreeValueCell::Inline {
                    is_compressed: true,
                    ..
                }
            ),
            "packing more raw values inline must not disable compression"
        );
        assert!(encoded.len() < value.len() / 4);
        assert_eq!(
            value_layout::decode(&pages, &encoded).expect("readback"),
            value
        );
    }
}

#[test]
fn medium_inline_values_keep_large_keys_and_overflow_replacements_intact() {
    let pages = Arc::new(Pages::default());
    let tree = BTree::new(Arc::clone(&pages));
    let key = |id: u64| {
        let mut key = vec![0x5a; reddb_engine::storage::btree::MAX_KEY_SIZE];
        key[..8].copy_from_slice(&id.to_be_bytes());
        key
    };
    let value = noise(4096);
    for id in 0..128 {
        tree.insert(&key(id), &value)
            .expect("maximum key and medium value");
    }
    let overflow_pages = || {
        pages
            .pages
            .lock()
            .expect("fixture lock")
            .values()
            .filter(|page| page.page_type().expect("page type") == PageType::Overflow)
            .count()
    };
    assert_eq!(overflow_pages(), 0);
    for _ in 0..6 {
        let overflow = noise(4097);
        tree.upsert(&key(64), &overflow)
            .expect("grow beyond inline capacity");
        assert_eq!(
            tree.get(&key(64)).expect("overflow readback"),
            Some(overflow)
        );
        assert_eq!(overflow_pages(), 1);
        tree.upsert(&key(64), &value)
            .expect("shrink back into shared leaf");
        assert_eq!(overflow_pages(), 0, "replaced chain must be reclaimed");
    }
    for id in 0..128 {
        assert_eq!(
            tree.get(&key(id)).expect("unchanged row"),
            Some(value.clone())
        );
    }
}

#[test]
fn ordered_cursor_peek_and_read_have_bounded_page_read_amplification() {
    let pages = Arc::new(Pages::default());
    let tree = Arc::new(BTree::new(Arc::clone(&pages)));
    let rows = 2000u64;
    for id in 1..=rows {
        tree.insert(&id.to_be_bytes(), b"cold").expect("fixture");
    }
    let tree_pages = pages.pages.lock().expect("fixture lock").len();
    let base = PagedCollection::new(tree);
    let mut cursor = base.scan().expect("cursor");
    pages.reads.store(0, Ordering::Relaxed);
    for id in 1..=rows {
        assert_eq!(cursor.peek_id().expect("peek"), Some(EntityId::new(id)));
        let record = cursor.next_record(4).expect("bounded read").expect("row");
        assert_eq!(record.id, EntityId::new(id));
        assert_eq!(record.bytes, b"cold");
    }
    assert!(cursor.peek_id().expect("end").is_none());
    let reads = pages.reads.load(Ordering::Relaxed);
    let rows = usize::try_from(rows).expect("fixture row count");
    eprintln!("ordered cursor: {reads} page reads, {rows} rows, {tree_pages} tree pages");
    assert!(
        reads <= 4 * tree_pages,
        "an immutable cursor must fetch each leaf once instead of re-reading and copying it for each row: {reads} reads, {rows} rows, {tree_pages} pages"
    );
}

#[test]
fn mutable_tree_cursor_keeps_observing_in_place_updates() {
    let tree = BTree::new(Arc::new(Pages::default()));
    for id in 1_u64..=3 {
        tree.insert(&id.to_be_bytes(), b"old").expect("fixture");
    }
    let mut cursor = tree.cursor_first().expect("mutable cursor");
    assert_eq!(
        cursor.peek_key().expect("peek"),
        Some(1_u64.to_be_bytes().to_vec())
    );
    tree.upsert(&1_u64.to_be_bytes(), b"new")
        .expect("in-place update");
    assert_eq!(
        cursor.next_with_budget(3).expect("updated entry"),
        Some((1_u64.to_be_bytes().to_vec(), b"new".to_vec()))
    );
    assert_eq!(
        cursor.peek_key().expect("peek"),
        Some(2_u64.to_be_bytes().to_vec())
    );
    tree.upsert(&2_u64.to_be_bytes(), b"changed")
        .expect("second update");
    assert_eq!(
        cursor.next_with_budget(7).expect("updated entry"),
        Some((2_u64.to_be_bytes().to_vec(), b"changed".to_vec()))
    );
}

#[test]
fn immutable_cursor_preserves_budget_retry_after_crossing_leaves() {
    let tree = Arc::new(BTree::new(Arc::new(Pages::default())));
    for id in 0_u64..32 {
        tree.insert(&id.to_be_bytes(), &noise(1000))
            .expect("fixture");
    }
    let large = vec![b'x'; 256 * 1024];
    tree.insert(&32_u64.to_be_bytes(), &large)
        .expect("large final row");
    let base = PagedCollection::new(tree);
    let mut cursor = base.scan().expect("immutable cursor");
    for id in 0_u64..32 {
        assert_eq!(cursor.peek_id().expect("peek"), Some(EntityId::new(id)));
        assert_eq!(
            cursor.next_record(2048).expect("entry").expect("row").id,
            EntityId::new(id)
        );
    }
    assert!(matches!(
        cursor.next_record(2048),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        cursor.peek_id().expect("same oversized row"),
        Some(EntityId::new(32))
    );
    let record = cursor
        .next_record(1024 * 1024)
        .expect("retry")
        .expect("row");
    assert_eq!(record.id, EntityId::new(32));
    assert_eq!(record.bytes, large);
    assert!(cursor.next_record(0).expect("end").is_none());
}

#[test]
fn immutable_cursor_rejects_corruption_when_entering_another_leaf() {
    let pages = Arc::new(Pages::default());
    let tree = Arc::new(BTree::new(Arc::clone(&pages)));
    for id in 1_u64..=2000 {
        tree.insert(&id.to_be_bytes(), b"cold").expect("fixture");
    }
    let mut images = pages.pages.lock().expect("fixture lock");
    let last_leaf = images
        .iter()
        .filter(|(_, page)| page.page_type().expect("fixture page type") == PageType::BTreeLeaf)
        .map(|(id, _)| *id)
        .max()
        .expect("leaf pages");
    images.insert(last_leaf, Page::new(PageType::Overflow, last_leaf));
    drop(images);
    let base = PagedCollection::new(tree);
    let mut cursor = base.scan().expect("first leaf remains valid");
    let mut rows = 0;
    loop {
        match cursor.next_record(4) {
            Ok(Some(_)) => rows += 1,
            Ok(None) => panic!("corrupt leaf must not look like end of data"),
            Err(StoreError::StorageIntegrity(_)) => break,
            Err(error) => panic!("unexpected failure: {error}"),
        }
    }
    assert!(
        rows > 0 && rows < 2000,
        "valid prefix followed by corrupt leaf"
    );
}

#[test]
fn raw_overflow_is_refused_before_reading_its_chain() {
    let pages = Arc::new(Pages::default());
    let tree = BTree::new(Arc::clone(&pages));
    let value = noise(256 * 1024);
    tree.insert(b"large", &value).expect("fixture insert");
    pages.overflow_reads.store(0, Ordering::Relaxed);
    assert!(matches!(
        tree.get_with_budget(b"large", 1024),
        Err(BTreeError::ReadLimitExceeded {
            limit_bytes: 1024,
            ..
        })
    ));
    assert_eq!(pages.overflow_reads.load(Ordering::Relaxed), 0);
    assert_eq!(
        tree.get_with_budget(b"large", value.len())
            .expect("admitted read"),
        Some(value)
    );
    assert_eq!(
        tree.get_with_budget(b"absent", 0).expect("missing key"),
        None
    );
}

#[test]
fn compressed_values_are_charged_for_expansion_before_decoding() {
    let pages = Arc::new(Pages::default());
    let tree = BTree::new(pages);
    for (key, value) in [
        (b"inline".as_slice(), vec![b'x'; 256 * 1024]),
        (b"spilled".as_slice(), noise(64 * 1024).repeat(4)),
    ] {
        tree.insert(key, &value).expect("fixture insert");
        assert!(matches!(
            tree.get_with_budget(key, 128 * 1024),
            Err(BTreeError::ReadLimitExceeded { .. })
        ));
        assert_eq!(
            tree.get_with_budget(key, 1024 * 1024)
                .expect("admitted read"),
            Some(value)
        );
    }
}

#[test]
fn cursor_limit_failure_does_not_skip_the_entry() {
    let tree = BTree::new(Arc::new(Pages::default()));
    for id in 0_u64..32 {
        tree.insert(&id.to_be_bytes(), &noise(1000))
            .expect("fixture insert");
    }
    let large = vec![b'x'; 256 * 1024];
    tree.insert(&32_u64.to_be_bytes(), &large)
        .expect("fixture insert");
    let mut cursor = tree.cursor_first().expect("fixture cursor");
    for id in 0_u64..32 {
        assert_eq!(
            cursor
                .next_with_budget(2048)
                .expect("small entry")
                .expect("entry")
                .0,
            id.to_be_bytes()
        );
    }
    assert!(matches!(
        cursor.next_with_budget(2048),
        Err(BTreeError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        cursor.next_with_budget(1024 * 1024).expect("retry entry"),
        Some((32_u64.to_be_bytes().to_vec(), large))
    );
    assert!(cursor.next_with_budget(0).expect("end of tree").is_none());
}

#[test]
fn merged_cursor_keeps_cold_records_and_applies_replacements_and_removals() {
    let pages = Arc::new(Pages::default());
    let tree = Arc::new(BTree::new(Arc::clone(&pages)));
    for id in 1_u64..=40 {
        tree.insert(&id.to_be_bytes(), &noise(1000))
            .expect("base record");
    }
    // Removing an oversized cold record must not materialize its payload.
    tree.insert(&41_u64.to_be_bytes(), &noise(256 * 1024))
        .expect("base overflow");
    let base = PagedCollection::new(tree);
    let mut changes = FrozenOverlay::new();
    changes.insert(EntityId::new(2), Some(Arc::from(b"replacement".as_slice())));
    changes.insert(EntityId::new(7), None);
    changes.insert(EntityId::new(41), None);
    changes.insert(EntityId::new(42), Some(Arc::from(b"new".as_slice())));
    let merged = MergedCollection::new(&base, Arc::new(changes));
    assert!(merged
        .get_record(EntityId::new(7), 0)
        .expect("removed record")
        .is_none());
    assert_eq!(
        merged
            .get_record(EntityId::new(2), 64)
            .expect("replacement")
            .expect("record")
            .bytes,
        b"replacement"
    );
    pages.overflow_reads.store(0, Ordering::Relaxed);
    let mut cursor = merged.cursor().expect("merged cursor");
    let mut ids = Vec::new();
    while let Some(record) = cursor.next_record(2048).expect("bounded record") {
        ids.push(record.id.raw());
    }
    let expected: Vec<_> = (1..=40).filter(|id| *id != 7).chain([42]).collect();
    assert_eq!(ids, expected);
    assert_eq!(pages.overflow_reads.load(Ordering::Relaxed), 0);
    assert_eq!(
        base.get_record(EntityId::new(2), 2048)
            .expect("unchanged base")
            .expect("record")
            .bytes,
        noise(1000)
    );
}

#[test]
fn merged_cursor_preserves_its_position_after_budget_denial() {
    let tree = Arc::new(BTree::new(Arc::new(Pages::default())));
    tree.insert(&1_u64.to_be_bytes(), b"base")
        .expect("base record");
    tree.insert(&2_u64.to_be_bytes(), b"cold")
        .expect("base record");
    let base = PagedCollection::new(tree);
    let mut changes = FrozenOverlay::new();
    changes.insert(EntityId::new(1), Some(Arc::from(vec![b'x'; 16384])));
    let merged = MergedCollection::new(&base, Arc::new(changes));
    let mut cursor = merged.cursor().expect("merged cursor");
    assert!(matches!(
        cursor.next_record(1024),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        cursor.peek_id().expect("same position"),
        Some(EntityId::new(1))
    );
    assert_eq!(
        cursor
            .next_record(16384)
            .expect("retry")
            .expect("record")
            .bytes
            .len(),
        16384
    );
    assert_eq!(
        cursor
            .next_record(4)
            .expect("cold record")
            .expect("record")
            .bytes,
        b"cold"
    );
    assert!(cursor.next_record(0).expect("end").is_none());
}

#[test]
fn native_record_identity_and_corrupt_keys_are_not_missing_data() {
    use reddb_file::native_store::STORE_VERSION_V11;
    let tree = Arc::new(BTree::new(Arc::new(Pages::default())));
    let row = UnifiedEntity::table_row(EntityId::new(3), "rows", 1, vec![]);
    let record = serialize_entity_record(&row, None, STORE_VERSION_V11);
    tree.insert(&3_u64.to_be_bytes(), &record)
        .expect("native row");
    tree.insert(&4_u64.to_be_bytes(), &record)
        .expect("inconsistent identity fixture");
    let base = PagedCollection::new(Arc::clone(&tree));
    let good = base
        .get_record(row.id, 4096)
        .expect("native record")
        .expect("record");
    assert_eq!(
        good.decode(STORE_VERSION_V11).expect("typed record").0.id,
        row.id
    );
    let inconsistent = base
        .get_record(EntityId::new(4), 4096)
        .expect("encoded record")
        .expect("record");
    assert!(matches!(
        inconsistent.decode(STORE_VERSION_V11),
        Err(StoreError::StorageIntegrity(_))
    ));
    tree.insert(b"invalid-key", &record)
        .expect("bad-key fixture");
    let mut cursor = base.scan().expect("cursor");
    assert!(cursor.skip_record().expect("first"));
    assert!(cursor.skip_record().expect("second"));
    assert!(matches!(
        cursor.peek_id(),
        Err(StoreError::StorageIntegrity(_))
    ));
}

#[test]
fn paged_cursor_outlives_the_read_handle_without_hydrating_the_collection() {
    let pages = Arc::new(Pages::default());
    let tree = Arc::new(BTree::new(Arc::clone(&pages)));
    for id in 1_u64..=1000 {
        tree.insert(&id.to_be_bytes(), b"cold")
            .expect("base record");
    }
    let base = PagedCollection::new(tree);
    let mut cursor = base.scan().expect("cursor");
    drop(base);
    for id in 1..=1000 {
        assert_eq!(cursor.peek_id().expect("key"), Some(EntityId::new(id)));
        assert_eq!(
            cursor
                .next_record(4)
                .expect("bounded record")
                .expect("record")
                .id,
            EntityId::new(id)
        );
    }
    assert!(cursor.next_record(0).expect("end").is_none());
}

#[test]
fn typed_record_budget_includes_native_expansion_and_metadata_containers() {
    use reddb_engine::storage::collection_read::EncodedRecord;
    use reddb_engine::storage::metadata::{Metadata, MetadataValue};
    use reddb_types::Value;
    let id = EntityId::new(7);
    let entity = UnifiedEntity::table_row(id, "rows", 7, vec![Value::text("x".repeat(32 * 1024))]);
    let record = EncodedRecord {
        id,
        bytes: serialize_entity_record(&entity, None, 11),
    };
    assert!(record.bytes.len() < 4096);
    assert!(matches!(
        record.decode_with_budget(11, 4096),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
    let (decoded, _) = record
        .decode_with_budget(11, 128 * 1024)
        .expect("admitted expanded value");
    assert_eq!(
        decoded.data.as_row().expect("decoded row").columns,
        entity.data.as_row().expect("original row").columns
    );

    let entity = UnifiedEntity::table_row(id, "rows", 7, vec![Value::Null]);
    let mut metadata = Metadata::new();
    metadata.set(
        "items",
        MetadataValue::Array(vec![MetadataValue::Null; 1024]),
    );
    let record = EncodedRecord {
        id,
        bytes: serialize_entity_record(&entity, Some(&metadata), 11),
    };
    assert!(record.bytes.len() < 4096);
    assert!(matches!(
        record.decode_with_budget(11, 4096),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        record
            .decode_with_budget(11, 256 * 1024)
            .expect("admitted metadata")
            .1
            .expect("metadata")
            .fields,
        metadata.fields
    );
}

#[test]
fn typed_record_budget_is_shared_across_all_columns() {
    use reddb_engine::storage::collection_read::EncodedRecord;
    use reddb_types::Value;
    let id = EntityId::new(1);
    let entity = UnifiedEntity::table_row(
        id,
        "rows",
        1,
        vec![Value::text("a".repeat(2048)), Value::text("b".repeat(2048))],
    );
    let record = EncodedRecord {
        id,
        bytes: serialize_entity_record(&entity, None, 11),
    };
    assert!(record.bytes.len() < 1024);
    assert!(
        matches!(
            record.decode_with_budget(11, 6000),
            Err(StoreError::ReadLimitExceeded { .. })
        ),
        "columns cannot each spend the complete decoder allowance"
    );
    assert!(record.decode_with_budget(11, 16 * 1024).is_ok());
}

#[test]
fn owned_merged_cursor_survives_its_views_and_preserves_retry_position() {
    let pages = Arc::new(Pages::default());
    let tree = Arc::new(BTree::new(Arc::clone(&pages)));
    for id in 1_u64..=4 {
        tree.insert(&id.to_be_bytes(), b"base").expect("base");
    }
    tree.insert(&5_u64.to_be_bytes(), &noise(256 * 1024))
        .expect("cold overflow");
    let base = PagedCollection::new(tree);
    let changes = FrozenOverlay::from([
        (EntityId::new(1), Some(Arc::from(vec![b'x'; 16384]))),
        (EntityId::new(2), None),
        (EntityId::new(5), None),
        (EntityId::new(6), Some(Arc::from(b"new".as_slice()))),
    ]);
    let merged = MergedCollection::new(&base, Arc::new(changes));
    let mut cursor = merged.owned_cursor().expect("owned cursor");
    drop(merged);
    drop(base);
    pages.overflow_reads.store(0, Ordering::Relaxed);
    assert!(matches!(
        cursor.next_record(1024),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        cursor.peek_id().expect("same position"),
        Some(EntityId::new(1))
    );
    assert_eq!(
        cursor
            .next_record(16384)
            .expect("retry")
            .expect("replacement")
            .bytes
            .len(),
        16384
    );
    let mut ids = Vec::new();
    while let Some(record) = cursor.next_record(4).expect("remaining bounded rows") {
        ids.push(record.id.raw());
    }
    assert_eq!(ids, vec![3, 4, 6]);
    assert_eq!(pages.overflow_reads.load(Ordering::Relaxed), 0);
}

#[test]
fn cold_key_projection_keeps_snapshot_history_and_masks_replaced_base_rows() {
    use reddb_engine::storage::entity::{EntityData, RowData};
    use reddb_engine::storage::manager::SegmentManager;
    use reddb_engine::transaction::read::SnapshotContext;
    use reddb_engine::transaction::snapshot::{Snapshot, SnapshotManager};
    use reddb_types::Value;
    use std::collections::HashSet;

    let make = |id, key: &str, xmin, xmax| {
        let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![Value::text(key)],
            vec!["key".into()],
        ));
        entity.set_xmin(xmin);
        entity.set_xmax(xmax);
        entity
    };
    let tree = Arc::new(BTree::new(Arc::new(Pages::default())));
    for entity in [
        make(1, "history", 3, 12),
        make(2, "history", 12, 0),
        make(3, "deleted", 3, 7),
        make(4, "renamed", 3, 0),
        make(5, "removed", 3, 0),
    ] {
        tree.insert(
            &entity.id.raw().to_be_bytes(),
            &serialize_entity_record(&entity, None, 11),
        )
        .expect("base version");
    }
    let manager = SegmentManager::new("keys");
    manager.install_backing(Arc::new(PagedCollection::new(tree)), 0, 11, 128 * 1024, 5);
    let snapshot = SnapshotContext {
        snapshot: Snapshot {
            xid: 10,
            in_progress: HashSet::new(),
        },
        manager: Arc::new(SnapshotManager::new()),
        own_xids: HashSet::new(),
        requires_index_fallback: false,
        serializable_reader: None,
    };
    let lookup = |key, snapshot| {
        manager
            .try_project_key_entity(key, snapshot, true, 128 * 1024, |entity| entity.id)
            .expect("keyed read")
    };
    assert_eq!(lookup("history", Some(&snapshot)), Some(EntityId::new(1)));
    assert_eq!(lookup("history", None), Some(EntityId::new(2)));
    assert_eq!(lookup("deleted", None), None);
    manager
        .update(make(4, "replacement", 3, 0))
        .expect("rename key in overlay");
    manager.delete(EntityId::new(5)).expect("physical delete");
    assert_eq!(lookup("renamed", None), None);
    assert_eq!(lookup("replacement", None), Some(EntityId::new(4)));
    assert_eq!(lookup("removed", None), None);
    assert_eq!(lookup("missing", None), None);
    assert!(matches!(
        manager.try_project_key_entity("history", None, true, 1, |_| ()),
        Err(StoreError::ReadLimitExceeded { .. })
    ));
}

#[test]
fn cold_key_projection_propagates_failed_base_io_even_with_a_resident_version() {
    use reddb_engine::storage::entity::{EntityData, RowData};
    use reddb_engine::storage::manager::SegmentManager;
    use reddb_types::Value;

    struct Unreadable;
    impl CollectionRead for Unreadable {
        fn get_record(
            &self,
            _: EntityId,
            _: usize,
        ) -> Result<Option<reddb_engine::storage::collection_read::EncodedRecord>, StoreError>
        {
            Ok(None)
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
            Err(std::io::Error::other("injected page read failure").into())
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
            Err(std::io::Error::other("injected page read failure").into())
        }
    }
    let manager = SegmentManager::new("keys");
    manager.install_backing(Arc::new(Unreadable), 0, 11, 128 * 1024, 1);
    let mut entity = UnifiedEntity::table_row(EntityId::new(2), "keys", 2, vec![]);
    entity.data = EntityData::Row(RowData::with_names(
        vec![Value::text("key")],
        vec!["key".into()],
    ));
    manager.insert(entity).expect("resident candidate");
    assert!(matches!(
        manager.try_project_key_entity("key", None, true, 128 * 1024, |entity| entity.id),
        Err(StoreError::Io(_))
    ));
    assert!(matches!(
        manager.try_project_key_entity("missing", None, false, 128 * 1024, |_| ()),
        Err(StoreError::Io(_))
    ));
}

#[test]
fn cold_physical_cursor_keeps_hidden_versions_and_their_metadata() {
    use reddb_engine::storage::manager::SegmentManager;
    use reddb_engine::storage::metadata::Metadata;
    let tree = Arc::new(BTree::new(Arc::new(Pages::default())));
    for id in 1_u64..=2 {
        let mut entity = UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]);
        entity.set_xmax(if id == 1 { 7 } else { 0 });
        let mut metadata = Metadata::new();
        metadata.set(
            "version",
            reddb_engine::storage::metadata::MetadataValue::Int(id as i64),
        );
        tree.insert(
            &id.to_be_bytes(),
            &serialize_entity_record(&entity, Some(&metadata), 11),
        )
        .expect("version");
    }
    let manager = SegmentManager::new("rows");
    manager.install_backing(Arc::new(PagedCollection::new(tree)), 0, 11, 128 * 1024, 2);
    let visible: Vec<_> = manager
        .try_paged_entity_cursor(None)
        .expect("visible cursor")
        .expect("cold")
        .map(|record| record.expect("visible row").0.id.raw())
        .collect();
    assert_eq!(visible, vec![2]);
    let physical: Vec<_> = manager
        .try_paged_physical_cursor()
        .expect("physical cursor")
        .expect("cold")
        .map(|record| {
            let (entity, metadata) = record.expect("physical row");
            let metadata = metadata.expect("version metadata");
            assert_eq!(
                metadata.get("version"),
                Some(&reddb_engine::storage::metadata::MetadataValue::Int(
                    entity.id.raw() as i64
                ))
            );
            entity.id.raw()
        })
        .collect();
    assert_eq!(physical, vec![1, 2]);
}

#[test]
fn indexed_cold_keys_bound_reads_preserve_versions_and_verify_candidates() {
    use reddb_engine::storage::entity::{EntityData, RowData};
    use reddb_engine::storage::manager::SegmentManager;
    use reddb_engine::transaction::read::SnapshotContext;
    use reddb_engine::transaction::snapshot::{Snapshot, SnapshotManager};
    use reddb_types::Value;
    use std::collections::HashSet;
    let pages = Arc::new(Pages::default());
    let data = Arc::new(BTree::new(Arc::clone(&pages)));
    let keys = Arc::new(BTree::new(Arc::clone(&pages)));
    let wanted = reddb_file::native_key_index::fingerprint("wanted");
    for id in 1_u64..=2048 {
        let key = if (2..=4).contains(&id) {
            "wanted".to_string()
        } else {
            format!("other-{id}")
        };
        let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![
                Value::text(key.as_str()),
                if id == 1 {
                    Value::text("x".repeat(32 * 1024))
                } else {
                    Value::Integer(id as i64)
                },
            ],
            vec!["key".into(), "value".into()],
        ));
        entity.set_xmin(match id {
            3 => 3,
            4 => 12,
            5 => 99,
            _ => 1,
        });
        if id == 3 {
            entity.set_xmax(12);
        }
        data.insert(
            &id.to_be_bytes(),
            &serialize_entity_record(&entity, None, 11),
        )
        .expect("record");
        // ID 5 simulates a fingerprint collision: it must not become a key match.
        let fingerprint = if id == 5 {
            wanted
        } else {
            reddb_file::native_key_index::fingerprint(&key)
        };
        keys.insert(
            &reddb_file::native_key_index::encode_candidate(&fingerprint, id),
            &[],
        )
        .expect("candidate");
    }
    let manager = SegmentManager::new("keys");
    manager.install_backing(
        Arc::new(PagedCollection::with_key_index(
            Arc::clone(&data),
            Arc::clone(&keys),
        )),
        0,
        11,
        128 * 1024,
        2048,
    );
    pages.reads.store(0, Ordering::Relaxed);
    assert_eq!(
        manager
            .try_project_key_entity("wanted", None, true, 4096, |entity| entity.id)
            .expect("indexed key"),
        Some(EntityId::new(4))
    );
    assert!(
        pages.reads.load(Ordering::Relaxed) < 100,
        "an indexed probe must not walk unrelated collection pages"
    );
    let snapshot = SnapshotContext {
        snapshot: Snapshot {
            xid: 10,
            in_progress: HashSet::new(),
        },
        manager: Arc::new(SnapshotManager::new()),
        own_xids: HashSet::new(),
        requires_index_fallback: false,
        serializable_reader: None,
    };
    assert_eq!(
        manager
            .try_project_key_entity("wanted", Some(&snapshot), true, 4096, |entity| entity.id)
            .expect("historical candidate"),
        Some(EntityId::new(3))
    );
    // A candidate referencing a missing physical row is corruption, not absence.
    data.delete(&4_u64.to_be_bytes())
        .expect("injected broken index reference");
    assert!(matches!(
        manager.try_project_key_entity("wanted", None, true, 4096, |_| ()),
        Err(StoreError::StorageIntegrity(_))
    ));
}

#[test]
fn owned_key_history_freezes_metadata_and_masks_renamed_or_deleted_base_rows() {
    use reddb_engine::storage::entity::{EntityData, RowData};
    use reddb_engine::storage::manager::SegmentManager;
    use reddb_engine::storage::metadata::{Metadata, MetadataValue};
    use reddb_types::Value;

    let entity = |id: u64, key: &str, value: Value| {
        let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![Value::text(key), value],
            vec!["key".into(), "value".into()],
        ));
        entity
    };
    let pages = Arc::new(Pages::default());
    let data = Arc::new(BTree::new(Arc::clone(&pages)));
    let keys = Arc::new(BTree::new(pages));
    for id in 1_u64..=5 {
        let key = if id == 1 || id == 4 {
            "other"
        } else {
            "wanted"
        };
        let mut row = entity(
            id,
            key,
            if id == 1 {
                Value::text("x".repeat(32 * 1024))
            } else {
                Value::Integer(id as i64)
            },
        );
        if id == 2 {
            row.set_xmax(7);
        }
        data.insert(&id.to_be_bytes(), &serialize_entity_record(&row, None, 11))
            .expect("base");
        // Include a deliberately colliding fingerprint for a different key.
        let fingerprint =
            reddb_file::native_key_index::fingerprint(if id == 4 { "wanted" } else { key });
        keys.insert(
            &reddb_file::native_key_index::encode_candidate(&fingerprint, id),
            &[],
        )
        .expect("key");
    }
    let manager = SegmentManager::new("keys");
    manager.install_backing(
        Arc::new(PagedCollection::with_key_index(Arc::clone(&data), keys)),
        0,
        11,
        4096,
        5,
    );
    manager
        .update(entity(3, "renamed", Value::text("x".repeat(32 * 1024))))
        .expect("renamed base row");
    manager.delete(EntityId::new(5)).expect("deleted base row");
    manager
        .insert(entity(7, "wanted", Value::Integer(7)))
        .expect("matching overlay");
    manager
        .insert(entity(8, "unrelated", Value::text("x".repeat(32 * 1024))))
        .expect("large dirty row");
    manager.set_column_schema_if_empty(vec!["key".into(), "value".into()]);
    manager
        .insert(UnifiedEntity::table_row(
            EntityId::new(10),
            "keys",
            10,
            vec![Value::text("wanted"), Value::Integer(10)],
        ))
        .expect("positional dirty row");
    let mut metadata = Metadata::new();
    metadata.set("version", MetadataValue::Int(1));
    manager
        .set_metadata(EntityId::new(7), metadata.clone())
        .expect("metadata");

    let frozen = manager
        .try_paged_key_physical_cursor("wanted")
        .expect("key cursor")
        .expect("cold");
    let visible: Vec<_> = manager
        .try_paged_key_entity_cursor("wanted", None)
        .expect("visible cursor")
        .expect("cold")
        .map(|record| record.expect("visible row").0.id.raw())
        .collect();
    assert_eq!(visible, vec![7, 10]);
    let snapshot = reddb_engine::transaction::read::SnapshotContext {
        snapshot: reddb_engine::transaction::snapshot::Snapshot {
            xid: 5,
            in_progress: Default::default(),
        },
        manager: Arc::new(reddb_engine::transaction::snapshot::SnapshotManager::new()),
        own_xids: Default::default(),
        requires_index_fallback: false,
        serializable_reader: None,
    };
    let historical: Vec<_> = manager
        .try_paged_key_entity_cursor("wanted", Some(snapshot))
        .expect("snapshot cursor")
        .expect("cold")
        .map(|record| record.expect("snapshot row").0.id.raw())
        .collect();
    assert_eq!(historical, vec![2, 7, 10]);
    metadata.set("version", MetadataValue::Int(2));
    manager
        .set_metadata(EntityId::new(7), metadata)
        .expect("new metadata");
    manager
        .delete(EntityId::new(7))
        .expect("delete after capture");
    manager
        .insert(entity(9, "wanted", Value::Integer(9)))
        .expect("new version");
    let old: Vec<_> = frozen
        .map(|record| {
            let (entity, metadata) = record.expect("frozen physical history");
            if entity.id.raw() == 7 {
                assert_eq!(
                    metadata.expect("frozen metadata").get("version"),
                    Some(&MetadataValue::Int(1))
                );
            }
            entity.id.raw()
        })
        .collect();
    assert_eq!(old, vec![2, 7, 10]);
    let current: Vec<_> = manager
        .try_paged_key_physical_cursor("wanted")
        .expect("new cursor")
        .expect("cold")
        .map(|record| record.expect("current history").0.id.raw())
        .collect();
    assert_eq!(current, vec![2, 9, 10]);
    assert!(matches!(
        manager.try_paged_key_physical_cursor("unrelated"),
        Err(StoreError::ReadLimitExceeded { .. })
    ));

    let legacy = SegmentManager::new("keys");
    legacy.install_backing(Arc::new(PagedCollection::new(data)), 0, 11, 128 * 1024, 5);
    let fallback: Vec<_> = legacy
        .try_paged_key_physical_cursor("wanted")
        .expect("legacy cursor")
        .expect("cold")
        .map(|record| record.expect("unindexed fallback row").0.id.raw())
        .collect();
    assert_eq!(fallback, vec![2, 3, 5]);
}
