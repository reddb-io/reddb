use reddb::storage::UnifiedEntity;
use reddb::{RedDBOptions, RedDBRuntime};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static LARGEST_ALLOCATION: Cell<usize> = const { Cell::new(0) };
}

struct QueryAllocator;

fn record_allocation(bytes: usize) {
    if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
        LARGEST_ALLOCATION.with(|largest| largest.set(largest.get().max(bytes)));
    }
}

unsafe impl GlobalAlloc for QueryAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static QUERY_ALLOCATOR: QueryAllocator = QueryAllocator;

#[test]
fn exact_search_does_not_clone_catalog_payloads() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE VECTOR bounded_vectors DIM 2 METRIC cosine")
        .expect("collection");
    let store = runtime.db().store();
    // Fixture installation is not the behavior under test. Allocate real IDs
    // and populate the same store that the public query executor scans.
    for _ in 0..16_384 {
        let id = store.next_entity_id();
        store
            .insert(
                "bounded_vectors",
                UnifiedEntity::vector(id, "bounded_vectors", vec![1.0, 0.0]),
            )
            .expect("fixture vector");
    }
    let sql = "VECTOR SEARCH bounded_vectors SIMILAR TO [1,0] MODE EXACT LIMIT 3";
    // Warm catalog/planner setup with a different result-cache key.
    runtime
        .execute_query("VECTOR SEARCH bounded_vectors SIMILAR TO [1,0] MODE EXACT LIMIT 4")
        .expect("warm catalog");
    LARGEST_ALLOCATION.with(|largest| largest.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = runtime.execute_query(sql);
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
    let largest = LARGEST_ALLOCATION.with(Cell::get);
    println!("largest exact-query allocation: {largest} bytes");
    let result = result.expect("exact search");
    assert_eq!(result.result.records.len(), 3);
    let stats = result.result.stats.vector.expect("vector stats");
    assert_eq!(stats.exact_distance_evaluations, 16_384);
    assert_eq!(stats.peak_topk_entries, 3);
    // The planner must not clone the full catalog (over 4 MiB for this
    // fixture). Scoring outside segment locks intentionally retains the
    // 128 KiB ID list plus bounded payload batches for concurrent writers.
    assert!(
        largest < 256 * 1024,
        "largest query allocation: {largest} bytes"
    );
}
