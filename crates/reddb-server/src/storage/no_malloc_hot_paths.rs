use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

use super::cache::blob::{BlobCache, BlobCachePut};
use super::engine::page::{Page, PageType};
use super::engine::page_cache::PageCacheShard;
use super::unified::hash_index::HashIndex;
use super::unified::segment::{GrowingSegment, UnifiedSegment};
use super::unified::{EntityId, UnifiedEntity};
use super::wal::WalRecord;

thread_local! {
    static COUNTER: RefCell<AllocationCounter> = const { RefCell::new(AllocationCounter::new()) };
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        COUNTER.with(|counter| counter.borrow_mut().record_dealloc());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AllocationCount {
    allocs: usize,
    deallocs: usize,
}

struct AllocationCounter {
    enabled: Cell<bool>,
    allocs: Cell<usize>,
    deallocs: Cell<usize>,
}

impl AllocationCounter {
    const fn new() -> Self {
        Self {
            enabled: Cell::new(false),
            allocs: Cell::new(0),
            deallocs: Cell::new(0),
        }
    }

    fn record_alloc(&mut self) {
        if self.enabled.get() {
            self.allocs.set(self.allocs.get() + 1);
        }
    }

    fn record_dealloc(&mut self) {
        if self.enabled.get() {
            self.deallocs.set(self.deallocs.get() + 1);
        }
    }

    fn begin(&self) {
        assert!(
            !self.enabled.replace(true),
            "allocation counter cannot be nested"
        );
        self.allocs.set(0);
        self.deallocs.set(0);
    }

    fn finish(&self) -> AllocationCount {
        let count = AllocationCount {
            allocs: self.allocs.get(),
            deallocs: self.deallocs.get(),
        };
        self.enabled.set(false);
        count
    }

    fn disable(&self) {
        self.enabled.set(false);
    }
}

struct CounterGuard;

impl Drop for CounterGuard {
    fn drop(&mut self) {
        COUNTER.with(|counter| counter.borrow().disable());
    }
}

fn measure_allocations<T>(operation: impl FnOnce() -> T) -> (T, AllocationCount) {
    COUNTER.with(|counter| counter.borrow().begin());
    let _guard = CounterGuard;
    let result = operation();
    let count = COUNTER.with(|counter| counter.borrow().finish());
    (result, count)
}

struct AllocationException {
    reason: &'static str,
    follow_up_issue: u64,
}

struct CoveredOperation {
    name: &'static str,
    allowed_allocs: usize,
    exception: Option<AllocationException>,
    measure: fn() -> AllocationCount,
}

const COVERED_OPERATIONS: &[CoveredOperation] = &[
    CoveredOperation {
        name: "hash-index-point-read-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_hash_index_point_read_hit,
    },
    CoveredOperation {
        name: "growing-segment-flat-row-insert",
        allowed_allocs: 3,
        exception: Some(AllocationException {
            reason: "bulk_insert returns an allocated id vector and builds per-call flat insert bookkeeping",
            follow_up_issue: 1956,
        }),
        measure: measure_growing_segment_flat_row_insert,
    },
    CoveredOperation {
        name: "page-cache-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_page_cache_hit,
    },
    CoveredOperation {
        name: "blob-cache-l1-hit",
        allowed_allocs: 2,
        exception: Some(AllocationException {
            reason: "BlobCache::get builds the owned namespace/key lookup key on the hit path",
            follow_up_issue: 1956,
        }),
        measure: measure_blob_cache_l1_hit,
    },
    CoveredOperation {
        name: "wal-record-encode-into-group-commit-buffer",
        allowed_allocs: 0,
        exception: None,
        measure: measure_wal_record_encode_into_group_commit_buffer,
    },
];

#[test]
fn counting_allocator_measures_a_known_allocating_fixture() {
    let ((), count) = measure_allocations(|| {
        let value = Box::new(7_u64);
        assert_eq!(*value, 7);
    });

    assert_eq!(
        count.allocs, 1,
        "Box::new should register exactly one allocation"
    );
    assert_eq!(
        count.deallocs, 1,
        "dropping the Box inside the measured closure should register exactly one deallocation"
    );
}

#[test]
fn covered_storage_hot_paths_do_not_exceed_the_manifest_floor() {
    for op in COVERED_OPERATIONS {
        if op.allowed_allocs == 0 {
            assert!(
                op.exception.is_none(),
                "{} has a zero floor and must not carry an exception",
                op.name
            );
        } else {
            let exception = op
                .exception
                .as_ref()
                .unwrap_or_else(|| panic!("{} has a nonzero floor without an exception", op.name));
            assert!(
                !exception.reason.is_empty() && exception.follow_up_issue > 0,
                "{} has an incomplete allocation-floor exception",
                op.name
            );
        }
        let count = (op.measure)();
        assert_eq!(
            count.allocs, op.allowed_allocs,
            "{} allocated {} times; manifest floor is {}",
            op.name, count.allocs, op.allowed_allocs
        );
    }
}

fn measure_hash_index_point_read_hit() -> AllocationCount {
    let mut index = HashIndex::new(false);
    index.insert(b"id:42".to_vec(), EntityId::new(42)).unwrap();
    let key = b"id:42";

    let (ids, count) = measure_allocations(|| index.get(key));

    assert_eq!(ids, &[EntityId::new(42)]);
    count
}

fn measure_growing_segment_flat_row_insert() -> AllocationCount {
    let mut segment = GrowingSegment::new(1, "orders");
    segment
        .bulk_insert(vec![UnifiedEntity::table_row(
            EntityId::new(1),
            "orders",
            1,
            Vec::new(),
        )])
        .unwrap();
    let entity = UnifiedEntity::table_row(EntityId::new(2), "orders", 2, Vec::new());

    let (ids, count) = measure_allocations(|| segment.bulk_insert(vec![entity]));
    let ids = ids.unwrap();

    assert_eq!(ids, vec![EntityId::new(2)]);
    count
}

fn measure_page_cache_hit() -> AllocationCount {
    let cache = PageCacheShard::new(8);
    let page = Page::new(PageType::BTreeLeaf, 7);
    cache.insert(7, page);

    let (hit, count) = measure_allocations(|| cache.get(7));

    assert!(hit.is_some());
    count
}

fn measure_blob_cache_l1_hit() -> AllocationCount {
    let cache = BlobCache::with_defaults();
    cache
        .put("sessions", "abc", BlobCachePut::new(b"payload".to_vec()))
        .unwrap();

    let (hit, count) = measure_allocations(|| cache.get("sessions", "abc"));

    assert_eq!(hit.unwrap().value(), b"payload");
    count
}

fn measure_wal_record_encode_into_group_commit_buffer() -> AllocationCount {
    let record = WalRecord::Commit { tx_id: 9 };
    let mut group_commit_buffer = Vec::with_capacity(128);

    let ((), count) = measure_allocations(|| record.encode_into(&mut group_commit_buffer));

    assert!(!group_commit_buffer.is_empty());
    count
}
