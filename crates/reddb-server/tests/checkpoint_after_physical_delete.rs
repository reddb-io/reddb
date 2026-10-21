//! Physical deletion already updates the paged tree; unchanged survivors need no re-encoding.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
use reddb_types::Value;

struct TrackedAllocator;

#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

fn account(bytes: usize) {
    if TRACK.load(Ordering::Relaxed) {
        ALLOCATED.fetch_add(bytes, Ordering::Relaxed);
    }
}

// Count allocator traffic only within the measured checkpoint. Delegation keeps
// System's layouts, ownership and failure behavior unchanged.
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            account(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            account(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            account(size);
        }
        replacement
    }
}

#[test]
fn physical_delete_checkpoint_does_not_reencode_wide_survivors() {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    let store = UnifiedStore::open_with_config(
        &path,
        UnifiedStoreConfig::default().with_page_cache_slots(8),
    )
    .expect("open paged store");
    store.create_collection("rows").expect("create collection");
    for id in 1..=128 {
        store
            .insert(
                "rows",
                UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text("x".repeat(64 * 1024))],
                ),
            )
            .expect("seed wide row");
    }
    store.persist().expect("initial checkpoint");
    assert_eq!(
        store
            .delete_batch(
                "rows",
                &[EntityId::new(1), EntityId::new(64), EntityId::new(128)]
            )
            .expect("physical deletes"),
        [EntityId::new(1), EntityId::new(64), EntityId::new(128)]
    );

    ALLOCATED.store(0, Ordering::Relaxed);
    TRACK.store(true, Ordering::Relaxed);
    let checkpoint = store.persist();
    TRACK.store(false, Ordering::Relaxed);
    checkpoint.expect("checkpoint after physical deletion");
    let allocated = ALLOCATED.load(Ordering::Relaxed);
    eprintln!("checkpoint_after_delete_allocated_bytes={allocated}");
    assert!(
        allocated < 4 * 1024 * 1024,
        "checkpoint re-encoded unchanged wide survivors: {allocated} allocated bytes"
    );

    // A direct manager edit is not covered by the preceding paged generation.
    // A later physical delete must not hide that edit by advancing its proof.
    store
        .get_collection("rows")
        .expect("collection")
        .update(UnifiedEntity::table_row(
            EntityId::new(2),
            "rows",
            2,
            vec![Value::text("changed")],
        ))
        .expect("untracked manager edit");
    assert_eq!(
        store
            .delete_batch("rows", &[EntityId::new(3)])
            .expect("delete after untracked edit"),
        [EntityId::new(3)]
    );
    store
        .persist()
        .expect("checkpoint must retain untracked edit");
    drop(store);
    let reopened = UnifiedStore::open(&path).expect("reopen checkpoint");
    assert_eq!(
        reopened.get_collection("rows").expect("collection").count(),
        124
    );
    for id in [1, 3, 64, 128] {
        assert!(reopened.get("rows", EntityId::new(id)).is_none());
    }
    assert_eq!(
        reopened
            .get("rows", EntityId::new(2))
            .expect("changed row")
            .data
            .as_row()
            .expect("row")
            .columns,
        [Value::text("changed")]
    );
    let survivor = reopened.get("rows", EntityId::new(4)).expect("survivor");
    assert_eq!(
        survivor.data.as_row().expect("row").columns,
        [Value::text("x".repeat(64 * 1024))]
    );
}
