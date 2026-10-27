//! Fallible reads are required before cold entities can stop owning resident payloads.
use reddb_engine::storage::{
    entity::{EntityId, UnifiedEntity},
    error::StoreError,
    metadata::{Metadata, MetadataValue},
    metadata_codec::serialize_entity_record,
    page::{Page, PageType, PAGE_SIZE},
    page_store::PageStore,
    paged_collection::{PagedCollectionReader, PagedEntityRecord},
    pager::PagerError,
};
use reddb_server::storage::{
    engine::{BTree, Pager, PagerConfig},
    memory_budget::resolve_for_boot,
    memory_pools::BudgetShares,
    DeployProfile, UnifiedStore,
};
use reddb_types::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

struct TrackedAllocator;

#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

fn account_allocated(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

// Test-only accounting delegates every allocation to System with its original
// layout; it never allocates bookkeeping objects or changes pointer ownership.
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            account_allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            account_allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            if size >= layout.size() {
                account_allocated(size - layout.size());
            } else {
                LIVE_BYTES.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        replacement
    }
}

static TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
const READER_BUDGET_BYTES: usize = 2 * 1024 * 1024;

struct ObservedPages {
    pager: Arc<Pager>,
    reads: AtomicUsize,
    fail_reads: AtomicBool,
}

impl PageStore for ObservedPages {
    fn read_page(&self, page_id: u32) -> Result<Page, PagerError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fail_reads.load(Ordering::Relaxed) {
            return Err(PagerError::Io(std::io::Error::other(
                "injected cold read failure",
            )));
        }
        self.pager.read_page(page_id)
    }
    fn write_page(&self, _: u32, _: Page) -> Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn allocate_page(&self, _: PageType) -> Result<Page, PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn free_page(&self, _: u32) -> Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.pager.page_count()
    }
}

fn payload(id: u64) -> Vec<u8> {
    let mut state = id + 1;
    (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn fixture_record(id: u64) -> (UnifiedEntity, Metadata) {
    let mut entity = UnifiedEntity::table_row(
        EntityId::new(id),
        "rows",
        id,
        vec![Value::UnsignedInteger(id), Value::Blob(payload(id))],
    );
    entity.xmin = 17;
    entity.xmax = 29;
    entity.sequence_id = id + 100;
    let mut metadata = Metadata::new();
    metadata.set("fixture", MetadataValue::Int(42));
    (entity, metadata)
}

fn verify_record(record: PagedEntityRecord, id: u64) {
    let (expected, metadata) = fixture_record(id);
    assert_eq!(record.entity.id, expected.id);
    assert_eq!(record.entity.xmin, expected.xmin);
    assert_eq!(record.entity.xmax, expected.xmax);
    assert_eq!(record.entity.sequence_id, expected.sequence_id);
    assert_eq!(
        record.entity.data.as_row().expect("table row").columns,
        expected.data.as_row().expect("expected row").columns
    );
    assert_eq!(
        record.metadata.expect("retained metadata").fields,
        metadata.fields
    );
}

fn observed_tree(
    path: &std::path::Path,
    root: u32,
) -> (
    Arc<ObservedPages>,
    Arc<reddb_engine::storage::btree::BTree<ObservedPages>>,
) {
    // Match the runtime reproduction's actual share, including room for the
    // recovery workspace. A fixed 64-page cache alone claimed half its budget.
    let budget = resolve_for_boot(DeployProfile::Serverless, Some(READER_BUDGET_BYTES as u64))
        .expect("explicit reader budget");
    let cache_size = BudgetShares::resolve(budget, DeployProfile::Serverless).page_cache_slots();
    let pager = Arc::new(
        Pager::open(
            path,
            PagerConfig {
                cache_size,
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .expect("read-only cold pager"),
    );
    let pages = Arc::new(ObservedPages {
        pager,
        reads: AtomicUsize::new(0),
        fail_reads: AtomicBool::new(false),
    });
    let tree = Arc::new(reddb_engine::storage::btree::BTree::with_root(
        pages.clone(),
        root,
    ));
    (pages, tree)
}

#[test]
fn disk_reader_attaches_without_loading_rows_and_scans_the_complete_reopened_tree() {
    let _test = TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    const ROWS: u64 = 2048;
    let directory = tempfile::tempdir().expect("isolated cold fixture");
    let path = directory.path().join("cold-reader.rdb");
    let root;
    {
        let pager = Arc::new(
            Pager::open(
                &path,
                PagerConfig {
                    cache_size: 64,
                    ..PagerConfig::default()
                },
            )
            .expect("fixture pager"),
        );
        let tree = BTree::new(pager.clone());
        for id in 1..=ROWS {
            let (entity, metadata) = fixture_record(id);
            let record = serialize_entity_record(
                &entity,
                Some(&metadata),
                reddb_file::STORE_VERSION_CURRENT,
            );
            tree.insert(&id.to_be_bytes(), &record)
                .expect("fixture record");
        }
        root = tree.root_page_id();
        pager.flush().expect("durable tree");
    }
    // Reopen the actual file; no UnifiedStore or resident SegmentManager is involved.
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);
    let (pages, tree) = observed_tree(&path, root);
    let retirement_observer = Arc::downgrade(&tree);
    eprintln!(
        "pager_open: extra_live_bytes={} extra_peak_bytes={}",
        LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline),
        PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline)
    );
    let reader = PagedCollectionReader::new(tree, "rows", reddb_file::STORE_VERSION_CURRENT)
        .expect("supported format");
    assert_eq!(
        pages.reads.load(Ordering::Relaxed),
        0,
        "attachment must not scan rows"
    );
    for id in [1, ROWS / 2, ROWS] {
        verify_record(
            reader
                .get(EntityId::new(id))
                .expect("point read")
                .expect("row"),
            id,
        );
    }
    let point_reads = pages.reads.load(Ordering::Relaxed);
    eprintln!(
        "point_reads: extra_live_bytes={} extra_peak_bytes={}",
        LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline),
        PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline)
    );
    assert!(
        point_reads < 100,
        "three point reads scanned {point_reads} pages"
    );
    assert!(reader
        .get(EntityId::new(ROWS + 1))
        .expect("genuine miss")
        .is_none());
    let mut cursor = reader.cursor().expect("cold cursor");
    drop(reader); // The cursor owns its tree and its retirement pin.
    assert!(retirement_observer
        .upgrade()
        .expect("cursor pins source")
        .has_retirement_readers());
    for id in 1..=ROWS {
        verify_record(
            cursor
                .next()
                .expect("fallible scan")
                .expect("complete scan"),
            id,
        );
        assert!(pages.pager.cache_len() <= pages.pager.cache_capacity());
    }
    assert!(cursor.next().expect("clean EOF").is_none());
    assert!(ROWS as usize * 4096 > pages.pager.cache_capacity() * PAGE_SIZE * 4);
    let extra_peak_bytes = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!(
        "full_scan: extra_live_bytes={} extra_peak_bytes={extra_peak_bytes}",
        LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline)
    );
    assert!(
        extra_peak_bytes <= READER_BUDGET_BYTES,
        "cold reader allocated {extra_peak_bytes} extra bytes for an 8 MiB dataset"
    );
    eprintln!("cold disk reader extra_peak_bytes={extra_peak_bytes}");
    eprintln!("cold disk reader: rows={ROWS} payload_bytes={} cache_slots={} point_page_reads={point_reads}",
        ROWS * 4096, pages.pager.cache_capacity());
    drop(cursor);
    assert!(
        retirement_observer.upgrade().is_none(),
        "closing cursor releases source ownership"
    );
}

#[test]
fn disk_errors_propagate_and_fence_the_cursor_instead_of_looking_like_missing_rows() {
    let _test = TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory = tempfile::tempdir().expect("isolated error fixture");
    let path = directory.path().join("read-errors.rdb");
    let root;
    {
        let pager = Arc::new(Pager::open_default(&path).expect("fixture pager"));
        let tree = BTree::new(pager.clone());
        let (entity, metadata) = fixture_record(1);
        tree.insert(
            &1u64.to_be_bytes(),
            &serialize_entity_record(&entity, Some(&metadata), reddb_file::STORE_VERSION_CURRENT),
        )
        .expect("fixture record");
        root = tree.root_page_id();
        pager.flush().expect("durable fixture");
    }
    let (pages, tree) = observed_tree(&path, root);
    let reader = PagedCollectionReader::new(tree, "rows", reddb_file::STORE_VERSION_CURRENT)
        .expect("supported format");
    let mut cursor = reader.cursor().expect("position before failure");
    pages.fail_reads.store(true, Ordering::Relaxed);
    let error = reader
        .get(EntityId::new(1))
        .err()
        .expect("explicit point-read failure");
    assert!(error.to_string().contains("injected cold read failure"));
    let error = cursor.next().err().expect("explicit scan failure");
    assert!(error.to_string().contains("injected cold read failure"));
    pages.fail_reads.store(false, Ordering::Relaxed);
    assert!(
        cursor.next().is_err(),
        "failed cursor must not resume past a damaged record"
    );
    verify_record(
        reader
            .get(EntityId::new(1))
            .expect("recovered point read")
            .expect("row"),
        1,
    );
    verify_record(
        reader
            .cursor()
            .expect("fresh cursor")
            .next()
            .expect("fresh read")
            .expect("row"),
        1,
    );
}

#[test]
fn paged_open_rejects_an_entity_under_a_different_physical_key() {
    let _test = TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory = tempfile::tempdir().expect("isolated corrupt fixture");
    let path = directory.path().join("mismatched.rdb");
    {
        let store = UnifiedStore::open(&path).expect("fixture store");
        store.create_collection("rows").expect("fixture collection");
        let entity =
            UnifiedEntity::table_row(EntityId::new(1024), "rows", 1, vec![Value::Integer(42)]);
        store.insert("rows", entity.clone()).expect("fixture row");
        store.persist().expect("durable fixture");
        let pager = Arc::clone(store.pager().expect("native pager"));
        let page = pager.read_page(1).expect("paged catalog");
        let content = page.content();
        let header = reddb_file::decode_native_paged_metadata_header(content)
            .expect("catalog header")
            .expect("versioned catalog");
        assert_eq!(header.collection_count, 1);
        let mut position = reddb_file::METADATA_HEADER_BYTES;
        let root = reddb_file::decode_native_paged_collection_root(content, &mut position)
            .expect("collection root");
        let tree = BTree::with_root(pager.clone(), root.root_page);
        let mut wrong_entity = entity;
        wrong_entity.id = EntityId::new(2048);
        let record = serialize_entity_record(&wrong_entity, None, header.format_version);
        tree.upsert(&1024u64.to_be_bytes(), &record)
            .expect("inject key/payload mismatch");
        pager.flush().expect("durable corrupt fixture");
    }
    let result = UnifiedStore::open(&path);
    assert!(
        matches!(result, Err(StoreError::StorageIntegrity(_))),
        "a validly framed row under the wrong key must not be silently restored"
    );
}
