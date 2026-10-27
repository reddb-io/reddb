//! Checkpoint composition must preserve the cold source, not just a hot delta.
use super::pager::MemoryFile;
use reddb_engine::storage::{
    btree::BTree,
    checkpoint_tree::build_checkpoint_tree,
    collection_read::{CollectionReadChanges, CollectionReadView},
    entity::{EntityId, UnifiedEntity},
    metadata::{Metadata, MetadataValue},
    metadata_codec::serialize_entity_record,
    page::{Page, PageType},
    page_store::PageStore,
    paged_collection::{PagedCollectionReader, PagedEntityRecord},
    pager::{NoWal, Pager, PagerConfig, PagerError},
};
use reddb_types::Value;
use std::{cell::Cell, sync::Arc};

pub const CASES: &[(&str, fn())] = &[
    ("checkpoint_reuses_identical_cold_source", identical_source),
    (
        "checkpoint_preserves_cold_base_and_delta_after_reopen",
        changed_source,
    ),
    (
        "checkpoint_source_failure_does_not_return_partial_root",
        source_failure,
    ),
    (
        "checkpoint_host_projection_preserves_physical_versions",
        host_projection,
    ),
];

struct Pages {
    pager: Pager<MemoryFile, NoWal>,
    fail_reads: Cell<bool>,
}

impl PageStore for Pages {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        if self.fail_reads.get() {
            return Err(PagerError::Io(std::io::Error::other(
                "checkpoint source fault",
            )));
        }
        self.pager.read_page(id)
    }
    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        self.pager.write_page(id, page)
    }
    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        self.pager.allocate_page(kind)
    }
    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.pager.free_page(id)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.pager.page_count()
    }
}

fn open_pages(bytes: Option<Vec<u8>>) -> Arc<Pages> {
    let create = bytes.is_none();
    let file = bytes.map(MemoryFile::from_bytes).unwrap_or_default();
    Arc::new(Pages {
        pager: Pager::open_owned(
            file,
            "checkpoint.rdb".into(),
            PagerConfig {
                cache_size: 24,
                ..Default::default()
            },
            NoWal,
            create,
        )
        .expect("fixture pager"),
        fail_reads: Cell::new(false),
    })
}

fn record(id: u64, revision: i64) -> PagedEntityRecord {
    let mut entity = UnifiedEntity::table_row(
        EntityId::new(id),
        "rows",
        id,
        vec![
            Value::UnsignedInteger(id),
            Value::Integer(revision),
            Value::Blob(vec![42; 4096]),
        ],
    );
    entity.created_at = 123;
    entity.updated_at = 456;
    entity.xmin = 17;
    entity.xmax = 29; // Physical checkpoint input includes history hidden from ordinary scans.
    entity.sequence_id = id + 100;
    let mut metadata = Metadata::new();
    metadata.set("revision", MetadataValue::Int(revision));
    PagedEntityRecord {
        entity,
        metadata: Some(metadata),
    }
}

fn encode(entity: &UnifiedEntity, metadata: Option<&Metadata>) -> Vec<u8> {
    serialize_entity_record(entity, metadata, reddb_file::STORE_VERSION_CURRENT)
}

fn fixture() -> (
    Arc<Pages>,
    Arc<BTree<Pages>>,
    Arc<PagedCollectionReader<Pages>>,
) {
    let pages = open_pages(None);
    let tree = Arc::new(BTree::new(pages.clone()));
    for id in [10u64, 30, 50, 70] {
        let record = record(id, 1);
        tree.insert(
            &id.to_be_bytes(),
            &encode(&record.entity, record.metadata.as_ref()),
        )
        .expect("base record");
    }
    pages.pager.flush().expect("durable base");
    let reader = Arc::new(
        PagedCollectionReader::new(tree.clone(), "rows", reddb_file::STORE_VERSION_CURRENT)
            .expect("source"),
    );
    (pages, tree, reader)
}

fn encoded_rows(reader: &PagedCollectionReader<Pages>) -> Vec<(u64, Vec<u8>)> {
    let mut cursor = reader.cursor().expect("verification cursor");
    let mut rows = Vec::new();
    while let Some(record) = cursor.next().expect("complete verification") {
        rows.push((
            record.entity.id.raw(),
            encode(&record.entity, record.metadata.as_ref()),
        ));
    }
    rows
}

fn expected(ids: &[(u64, i64)]) -> Vec<(u64, Vec<u8>)> {
    ids.iter()
        .map(|(id, revision)| {
            let record = record(*id, *revision);
            (*id, encode(&record.entity, record.metadata.as_ref()))
        })
        .collect()
}

fn identical_source() {
    let (pages, tree, base) = fixture();
    let view =
        CollectionReadView::paged(base.clone(), Arc::new(CollectionReadChanges::new()), None);
    let before = pages.page_count().expect("page count");
    let built = build_checkpoint_tree(&pages, &view, Some(&tree), |entity, metadata| {
        Some(encode(entity, metadata))
    })
    .expect("identical checkpoint");
    assert!(
        Arc::ptr_eq(&built.tree, &tree),
        "identical complete input reuses the tree"
    );
    assert_eq!(built.peak_buffered_bytes, 0);
    assert_eq!(pages.page_count().expect("unchanged count"), before);
    assert_eq!(
        encoded_rows(&base),
        expected(&[(10, 1), (30, 1), (50, 1), (70, 1)])
    );
}

fn changed_source() {
    let (pages, tree, base) = fixture();
    let mut changes = CollectionReadChanges::new();
    changes.insert(EntityId::new(50), Some(Arc::new(record(50, 2))));
    changes.insert(EntityId::new(70), None);
    changes.insert(EntityId::new(90), Some(Arc::new(record(90, 2))));
    let view = CollectionReadView::paged(base.clone(), Arc::new(changes), None);
    let built = build_checkpoint_tree(&pages, &view, Some(&tree), |entity, metadata| {
        Some(encode(entity, metadata))
    })
    .expect("composed checkpoint");
    assert!(!Arc::ptr_eq(&built.tree, &tree));
    assert!(built.peak_buffered_bytes > 0);
    let root = built.tree.root_page_id();
    assert_eq!(
        encoded_rows(&base),
        expected(&[(10, 1), (30, 1), (50, 1), (70, 1)]),
        "unpublished replacement preserves the old generation"
    );
    pages.pager.flush().expect("flush fixture replacement");
    let bytes = pages
        .pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable bytes");
    drop(built);
    drop(view);
    drop(base);
    drop(tree);
    drop(pages);
    let reopened = open_pages(Some(bytes));
    let reader = PagedCollectionReader::new(
        Arc::new(BTree::with_root(reopened, root)),
        "rows",
        reddb_file::STORE_VERSION_CURRENT,
    )
    .expect("reopened source");
    assert_eq!(
        encoded_rows(&reader),
        expected(&[(10, 1), (30, 1), (50, 2), (90, 2)]),
        "unchanged prefix, replacement, deletion and append survive reopen"
    );
}

fn source_failure() {
    let (pages, tree, base) = fixture();
    let view =
        CollectionReadView::paged(base.clone(), Arc::new(CollectionReadChanges::new()), None);
    let original_root = tree.root_page_id();
    let mut visited = 0;
    let result = build_checkpoint_tree(&pages, &view, None, |entity, metadata| {
        visited += 1;
        pages.fail_reads.set(true);
        Some(encode(entity, metadata))
    });
    assert_eq!(visited, 1);
    assert!(
        result.is_err(),
        "a source failure cannot publish a prefix as a complete checkpoint"
    );
    pages.fail_reads.set(false);
    assert_eq!(tree.root_page_id(), original_root);
    assert_eq!(
        encoded_rows(&base),
        expected(&[(10, 1), (30, 1), (50, 1), (70, 1)])
    );
}

fn host_projection() {
    let (pages, tree, base) = fixture();
    let view = CollectionReadView::paged(base, Arc::new(CollectionReadChanges::new()), None);
    let built = build_checkpoint_tree(&pages, &view, Some(&tree), |entity, metadata| {
        if entity.id == EntityId::new(30) {
            return None;
        }
        let mut normalized = entity.clone();
        if entity.id == EntityId::new(50) {
            normalized.xmax = 0;
        }
        Some(encode(&normalized, metadata))
    })
    .expect("host visibility projection");
    let reader = PagedCollectionReader::new(built.tree, "rows", reddb_file::STORE_VERSION_CURRENT)
        .expect("projected reader");
    let rows = encoded_rows(&reader);
    assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [10, 50, 70]);
    let mut changed = record(50, 1);
    changed.entity.xmax = 0;
    assert_eq!(
        rows[1].1,
        encode(&changed.entity, changed.metadata.as_ref())
    );
    assert_eq!(
        reader
            .get(EntityId::new(10))
            .expect("history retained")
            .expect("present")
            .entity
            .xmax,
        29
    );
}
