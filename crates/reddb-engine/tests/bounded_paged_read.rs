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
}

impl PageStore for Pages {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
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
