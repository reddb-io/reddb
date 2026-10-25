//! Native header publication must not materialize every row's text at once.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

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
fn unrelated_ddl_and_checkpoint_do_not_clone_the_text_corpus() {
    let directory = tempfile::tempdir().expect("temporary native store");
    let path = directory.path().join("artifacts.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_memory_budget(256 * 1024 * 1024)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.result_cache.enabled = false")
        .expect("no result cache");
    runtime
        .execute_query("CREATE TABLE artifact_rows (id INTEGER, payload TEXT)")
        .expect("source table");
    let text = "x".repeat(64 * 1024);
    let value = Value::Text(text.clone().into());
    for first in (0..1024).step_by(64) {
        let mut parameters = Vec::new();
        let mut rows = Vec::new();
        for id in first..first + 64 {
            parameters.push(Value::Integer(id));
            parameters.push(value.clone());
            rows.push(format!(
                "(${}, ${})",
                parameters.len() - 1,
                parameters.len()
            ));
        }
        runtime
            .execute_query_with_params(
                &format!(
                    "INSERT INTO artifact_rows (id,payload) VALUES {}",
                    rows.join(",")
                ),
                &parameters,
            )
            .expect("seed source corpus");
    }
    runtime.checkpoint().expect("publish initial corpus");
    for operation in ["CREATE TABLE artifact_probe (id INTEGER)", "CHECKPOINT"] {
        let baseline = LIVE_BYTES.load(Ordering::Relaxed);
        PEAK_BYTES.store(baseline, Ordering::Relaxed);
        if operation == "CHECKPOINT" {
            runtime.checkpoint().expect("republish existing artifacts");
        } else {
            runtime.execute_query(operation).expect("unrelated DDL");
        }
        let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
        eprintln!("operation={operation} native_artifact_extra_peak_bytes={extra}");
        assert!(
            extra < 16 * 1024 * 1024,
            "native publication allocated {extra} transient bytes for an unchanged corpus"
        );
    }
    let store = runtime.db().store();
    let header = store.physical_file_header().expect("native header");
    let (_, bytes) = store
        .read_native_vector_artifact_blob(
            header.vector_artifact_page,
            "artifact_rows",
            Some("text.fulltext"),
        )
        .expect("persisted fulltext")
        .expect("artifact");
    let frame = reddb_file::decode_native_fulltext_frame(&bytes).expect("fulltext frame");
    assert_eq!(frame.doc_count, 1024);
    let term = frame
        .terms
        .iter()
        .find(|term| term.term == text)
        .expect("large term remains indexed");
    assert_eq!(term.postings.len(), 1024);
    drop(frame);
    drop(bytes);
    drop(store);
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("cold reopen");
    let count = recovered
        .execute_query("SELECT COUNT(*) AS n FROM artifact_rows")
        .expect("read rows");
    assert_eq!(
        count.result.records[0].get("n").expect("count").to_string(),
        "1024"
    );
}
