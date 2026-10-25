//! Flushing a resident page cache must not clone every dirty page at once.
use reddb_server::storage::engine::{PageType, Pager, PagerConfig};
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
fn pager_flush_bounds_staging_memory_and_cold_reopens_all_pages() {
    let directory = tempfile::tempdir().expect("temporary pager");
    let path = directory.path().join("flush.rdb");
    let pager = Pager::open(
        &path,
        PagerConfig {
            cache_size: 16384,
            ..Default::default()
        },
    )
    .expect("pager");
    let mut ids = Vec::new();
    for value in 0_u64..8192 {
        let mut page = pager
            .allocate_page(PageType::Overflow)
            .expect("allocate page");
        let id = page.page_id();
        page.content_mut()[..8].copy_from_slice(&value.to_le_bytes());
        pager.write_page(id, page).expect("dirty page");
        ids.push(id);
    }
    assert!(pager.dirty_page_count() >= ids.len());
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);
    pager.sync().expect("durable dirty flush");
    let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    eprintln!("pager_flush_extra_peak_bytes={extra}");
    assert!(
        extra < 8 * 1024 * 1024,
        "flush cloned {extra} transient bytes from a 128 MiB dirty cache"
    );
    assert_eq!(pager.dirty_page_count(), 0);
    drop(pager);
    let reopened = Pager::open_default(&path).expect("cold reopen");
    for (value, id) in ids.into_iter().enumerate() {
        assert_eq!(
            &reopened.read_page(id).expect("durable page").content()[..8],
            &(value as u64).to_le_bytes()
        );
    }
}
