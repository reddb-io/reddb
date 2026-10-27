//! LIMIT pushdown must not clone an entire resident candidate set first.
use reddb_engine::storage::{
    collection_read::CollectionReadView,
    entity::{EntityId, UnifiedEntity},
    manager::SegmentManager,
    page::{Page, PageType},
    page_store::PageStore,
    pager::PagerError,
};
use reddb_types::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

// Resident authority must not consult a page store.
struct NoPages;

impl PageStore for NoPages {
    fn read_page(&self, _: u32) -> Result<Page, PagerError> {
        unreachable!("resident read accessed pages")
    }
    fn write_page(&self, _: u32, _: Page) -> Result<(), PagerError> {
        unreachable!("resident read wrote pages")
    }
    fn allocate_page(&self, _: PageType) -> Result<Page, PagerError> {
        unreachable!("resident read allocated pages")
    }
    fn free_page(&self, _: u32) -> Result<(), PagerError> {
        unreachable!("resident read freed pages")
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        unreachable!("resident read counted pages")
    }
}

struct TrackedAllocator;

#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
static TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn account_allocated(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

// Test-only bookkeeping never allocates and preserves System's ownership/layout.
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

#[test]
fn indexed_candidate_reads_materialize_only_the_consumed_prefix() {
    let _gate = TEST_GATE.lock().expect("serial allocation observation");
    const ROWS: u64 = 2048;
    const PAYLOAD_BYTES: usize = 4096;
    const WORKSPACE_BYTES: usize = 128 * 1024;
    let manager = Arc::new(SegmentManager::new("rows"));
    for id in 1..=ROWS {
        manager
            .insert(UnifiedEntity::table_row(
                EntityId::new(id),
                "rows",
                id,
                vec![Value::Blob(vec![42; PAYLOAD_BYTES])],
            ))
            .expect("fixture row");
    }
    let view = CollectionReadView::<NoPages>::resident(manager);
    let ids: Vec<_> = (1..=ROWS).map(EntityId::new).collect();
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);

    let mut candidates = view.read_ids(&ids);
    let constructed = LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    let first = candidates
        .next()
        .expect("first candidate")
        .expect("read")
        .expect("present");
    assert_eq!(first.id, EntityId::new(1));
    assert_eq!(
        first.data.as_row().expect("row").columns[0],
        Value::Blob(vec![42; PAYLOAD_BYTES])
    );
    drop(first);
    drop(candidates); // A LIMIT of one must not materialize the remaining rows.

    let peak = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    let retained = LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!("rows={ROWS} payload_bytes={} construction_extra_bytes={constructed} peak_extra_bytes={peak} retained_extra_bytes={retained}", ROWS * PAYLOAD_BYTES as u64);
    assert!(
        constructed < WORKSPACE_BYTES,
        "eager candidate hydration: {constructed}"
    );
    assert!(
        peak < WORKSPACE_BYTES,
        "candidate prefix exceeded workspace: {peak}"
    );
    assert!(
        retained < WORKSPACE_BYTES,
        "discarded candidates retained payloads: {retained}"
    );
}

#[test]
fn scan_consumer_releases_locks_and_keeps_the_initial_append_boundary() {
    let _gate = TEST_GATE.lock().expect("serial allocation observation");
    let manager = Arc::new(SegmentManager::new("rows"));
    for id in 1..=2 {
        manager
            .insert(UnifiedEntity::table_row(
                EntityId::new(id),
                "rows",
                id,
                vec![],
            ))
            .expect("initial row");
    }
    let view = CollectionReadView::<NoPages>::resident(manager.clone());
    let mut seen = Vec::new();
    let mut writer_completed_during_callback = false;
    std::thread::scope(|scope| {
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let mut writer = None;
        assert!(view
            .scan_for_each(
                None,
                || true,
                |entity| {
                    seen.push(entity.id.raw());
                    if writer.is_none() {
                        let manager = manager.clone();
                        let sent = sent.clone();
                        writer = Some(scope.spawn(move || {
                            manager.force_seal().expect("seal while consumer executes");
                            manager
                                .insert(UnifiedEntity::table_row(
                                    EntityId::new(3),
                                    "rows",
                                    3,
                                    vec![],
                                ))
                                .expect("concurrent append");
                            let _ = sent.send(()); // The failing baseline can finish after receiver timeout.
                        }));
                        writer_completed_during_callback = received
                            .recv_timeout(std::time::Duration::from_secs(2))
                            .is_ok();
                    }
                    true
                }
            )
            .expect("complete scan"));
    }); // Join after scan releases its guards, including the failing baseline.
    assert!(
        writer_completed_during_callback,
        "scan consumer retained a topology/segment lock"
    );
    seen.sort_unstable();
    assert_eq!(
        seen,
        [1, 2],
        "late appends must not extend the opened cursor"
    );
    assert!(manager.get(EntityId::new(3)).is_some());
}

#[test]
fn owned_scan_streams_payloads_without_materializing_a_batch() {
    let _gate = TEST_GATE.lock().expect("serial allocation observation");
    const ROWS: usize = 2048;
    const PAYLOAD_BYTES: usize = 4096;
    const WORKSPACE_BYTES: usize = 128 * 1024;
    let manager = Arc::new(SegmentManager::new("rows"));
    for id in 1..=ROWS as u64 {
        manager
            .insert(UnifiedEntity::table_row(
                EntityId::new(id),
                "rows",
                id,
                vec![Value::Blob(vec![42; PAYLOAD_BYTES])],
            ))
            .expect("fixture row");
    }
    let view = CollectionReadView::<NoPages>::resident(manager);
    let mut seen = vec![false; ROWS + 1];
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);
    let mut consumed = 0;
    assert!(!view
        .scan_for_each(
            None,
            || true,
            |_| {
                consumed += 1;
                false
            }
        )
        .expect("early stop"));
    assert_eq!(consumed, 1);
    assert!(view
        .scan_for_each(
            None,
            || true,
            |entity| {
                let id = usize::try_from(entity.id.raw()).expect("fixture ID");
                assert!(!seen[id], "duplicate physical row");
                seen[id] = true;
                let Value::Blob(payload) = &entity.data.as_row().expect("row").columns[0] else {
                    panic!("blob payload")
                };
                assert_eq!(payload.len(), PAYLOAD_BYTES);
                assert!(payload.iter().all(|byte| *byte == 42));
                true
            }
        )
        .expect("complete owned scan"));
    assert!(seen[1..].iter().all(|seen| *seen));
    let peak = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    let retained = LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!("owned_scan rows={ROWS} payload_bytes={} peak_extra_bytes={peak} retained_extra_bytes={retained}", ROWS * PAYLOAD_BYTES);
    assert!(
        peak < WORKSPACE_BYTES,
        "owned scan hydrated a payload batch: {peak}"
    );
    assert!(
        retained < WORKSPACE_BYTES,
        "scan retained payloads: {retained}"
    );
}
