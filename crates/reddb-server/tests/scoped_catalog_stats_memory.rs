//! Scoped statistics must not materialize unrelated native collection payloads.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
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

fn empty_collection_stats_peak(runtime: &RedDBRuntime, query: &str) -> usize {
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);
    let result = runtime.execute_query(query).expect("scoped statistics");
    let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    assert!(
        result.result.records.iter().any(|row| {
            row.get("metric") == Some(&Value::text("row_count"))
                && row.get("value") == Some(&Value::UnsignedInteger(0))
        }),
        "empty collection statistics: {:?}",
        result.result
    );
    extra
}

#[test]
fn scoped_statistics_do_not_materialize_unrelated_native_json_rows() {
    let directory = tempfile::tempdir().expect("owned native fixture");
    let options = RedDBOptions::persistent(directory.path().join("database.rdb"))
        .with_memory_budget(512 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("native paged profile");
    let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.result_cache.enabled = false")
        .expect("disable result cache");
    runtime
        .execute_query("CREATE DOCUMENT empty_documents")
        .expect("document target");
    runtime
        .execute_query("CREATE TABLE empty_table (id INTEGER)")
        .expect("table target");
    let control = empty_collection_stats_peak(&runtime, "SHOW STATS empty_documents");
    runtime
        .execute_query("CREATE TABLE unrelated_rows (id INTEGER, payload JSON)")
        .expect("unrelated table");
    let payload = format!("{{\"value\":\"{}\"}}", "x".repeat(64 * 1024)).into_bytes();
    for first in (0_i64..256).step_by(32) {
        let mut values = Vec::new();
        let mut params = Vec::new();
        for id in first..first + 32 {
            params.push(Value::Integer(id));
            params.push(Value::Json(payload.clone()));
            values.push(format!("(${}, ${})", params.len() - 1, params.len()));
        }
        runtime
            .execute_query_with_params(
                &format!(
                    "INSERT INTO unrelated_rows (id,payload) VALUES {}",
                    values.join(",")
                ),
                &params,
            )
            .expect("native JSON corpus");
    }
    for query in [
        "SHOW STATS empty_table",
        "SHOW STATS empty_documents",
        "SELECT * FROM red.stats WHERE collection = 'empty_documents' AND metric = 'row_count'",
        "SELECT * FROM red.stats WHERE metric = 'row_count' AND (collection = 'empty_table' OR collection = 'empty_documents')",
    ] {
        let loaded = empty_collection_stats_peak(&runtime, query);
        eprintln!("query={query} control_peak_bytes={control} loaded_peak_bytes={loaded}");
        assert!(loaded < control.saturating_add(2 * 1024 * 1024),
            "scoped statistics allocated {loaded} bytes against {control} control while targets remained empty");
    }
    let disjunction = runtime.execute_query(
        "SELECT collection,value FROM red.stats WHERE metric = 'row_count' AND (collection = 'empty_documents' OR collection = 'unrelated_rows')",
    ).expect("disjunction must keep both collections");
    assert_eq!(disjunction.result.records.len(), 2);
    for (collection, count) in [("empty_documents", 0), ("unrelated_rows", 256)] {
        assert!(disjunction.result.records.iter().any(|row| {
            row.get("collection") == Some(&Value::text(collection))
                && row.get("value") == Some(&Value::UnsignedInteger(count))
        }));
    }
    runtime.checkpoint().expect("publish fixture");
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options).expect("cold reopen");
    let count = recovered
        .execute_query("SELECT COUNT(*) AS n FROM unrelated_rows")
        .expect("rows recovered");
    assert_eq!(
        count.result.records[0].get("n").expect("count").to_string(),
        "256"
    );
    let cold_peak = empty_collection_stats_peak(&recovered, "SHOW STATS empty_documents");
    assert!(
        cold_peak < control.saturating_add(2 * 1024 * 1024),
        "cold stats peak: {cold_peak}"
    );
}
