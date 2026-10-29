//! Metadata hashing must not allocate a rendered copy of an unchanged field.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackedAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;

unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        replacement
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[test]
fn cold_metadata_fingerprint_streams_an_unchanged_large_field() {
    let directory = tempfile::tempdir().expect("isolated metadata fixture");
    let path = directory.path().join("fingerprint.rdb");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_memory_budget(512 * 1024 * 1024)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile"),
    )
    .expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "SET CONFIG runtime.result_cache.enabled = false",
        "CREATE TABLE fingerprint_rows (id INT,payload TEXT)",
    ] {
        runtime.execute_query(query).expect("fixture setup");
    }
    // Single-character tokens avoid creating a separate giant term artifact.
    let payload = "x ".repeat(2 * 1024 * 1024);
    runtime
        .execute_query_with_params(
            "INSERT INTO fingerprint_rows (id,payload) VALUES (1,$1)",
            &[Value::text(payload.clone())],
        )
        .expect("large source field");
    runtime
        .checkpoint()
        .expect("initial source and native publication");
    let before = runtime
        .db()
        .native_physical_state()
        .expect("native state")
        .collection_roots["fingerprint_rows"];
    let manager = runtime
        .db()
        .store()
        .get_collection("fingerprint_rows")
        .expect("source manager");
    // Invalidate only the memoized metadata fingerprint, keeping row revisions
    // and native artifact proofs unchanged, so the allocation window is real
    // metadata rendering rather than full artifact construction.
    manager.cached_physical_root(0, || 0);
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    runtime
        .checkpoint()
        .expect("recompute metadata fingerprint");
    let extra = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!("metadata_fingerprint_extra_peak_bytes={extra}");
    let after = runtime
        .db()
        .native_physical_state()
        .expect("published state")
        .collection_roots["fingerprint_rows"];
    assert_eq!(
        after, before,
        "unchanged source must retain its exact fingerprint"
    );
    assert!(
        extra < 2 * 1024 * 1024,
        "metadata hashing allocated {extra} extra bytes for a 4 MiB field"
    );
    drop(manager);
    drop(runtime);
    let recovered =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("cold reopen");
    let rows = recovered
        .execute_query("SELECT payload FROM fingerprint_rows WHERE id=1")
        .expect("recovered source row")
        .result
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("payload").expect("payload").as_text(),
        Some(payload.as_str())
    );
}
