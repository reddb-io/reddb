//! A small result window must not clone the skipped rows or the whole table.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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

#[test]
fn limited_offset_scan_keeps_materialization_bounded() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory fixture opens");
    runtime
        .execute_query("SET CONFIG runtime.result_cache.enabled = false")
        .expect("disable result caching for the allocation measurement");
    runtime
        .execute_query("CREATE TABLE wide_rows (id INTEGER, payload JSON)")
        .expect("create fixture table");
    let payload = format!("{{\"value\":\"{}\"}}", "x".repeat(16 * 1024)).into_bytes();
    for start in (0_i64..5_000).step_by(100) {
        let mut params = Vec::with_capacity(200);
        let mut values = Vec::with_capacity(100);
        for id in start..start + 100 {
            params.push(Value::Integer(id));
            params.push(Value::Json(payload.clone()));
            values.push(format!("(${}, ${})", params.len() - 1, params.len()));
        }
        runtime
            .execute_query_with_params(
                &format!(
                    "INSERT INTO wide_rows (id, payload) VALUES {}",
                    values.join(",")
                ),
                &params,
            )
            .expect("insert fixture batch");
        if (start + 100) % 1_000 == 0 {
            eprintln!("seeded_rows={}", start + 100);
        }
    }
    // Capture the storage scan's own order, without imposing a sort that would
    // legitimately require materializing input outside the requested window.
    let all = runtime
        .execute_query("SELECT id FROM wide_rows")
        .expect("read fixture identifiers");
    assert_eq!(all.result.records.len(), 5_000);
    let expected = all.result.records[4_095]
        .get("id")
        .expect("selected identifier exists")
        .clone();
    drop(all);

    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);
    let page = runtime
        .execute_query("SELECT id, payload FROM wide_rows LIMIT 1 OFFSET 4095")
        .expect("read a one-row window across the parallel-scan threshold");
    let extra_peak = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!("limited_offset_extra_peak_bytes={extra_peak}");
    assert_eq!(page.result.records.len(), 1);
    assert_eq!(page.result.records[0].get("id"), Some(&expected));
    assert!(
        extra_peak < 16 * 1024 * 1024,
        "one-row OFFSET query allocated {extra_peak} extra bytes"
    );
}
