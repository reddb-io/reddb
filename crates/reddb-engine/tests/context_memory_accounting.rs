//! Independently measure allocations: accounting must cover live index storage.
use reddb_engine::storage::context_index::ContextIndex;
use reddb_engine::storage::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb_types::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackedAllocator;
#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// Test-only observer: System receives the original pointer, layout and size.
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
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
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
}

#[test]
fn context_bounds_cover_measured_live_allocations_and_mutation_peaks() {
    let index = ContextIndex::new();
    let baseline = LIVE.load(Ordering::Relaxed);
    index.set_collection_enabled("docs", true);
    for id in 1..=1000 {
        let index_live = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
        let already_charged = index.memory_bytes();
        let entity = UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TableRow {
                table: "docs".into(),
                row_id: id,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(
                    (0..12)
                        .map(|field| {
                            (
                                format!("field{field}"),
                                Value::text(format!(
                                    "customer{id} reference{field} shared alpha beta gamma"
                                )),
                            )
                        })
                        .collect(),
                ),
                schema: None,
            }),
        );
        let bound = index.estimate_entity_growth("docs", &entity);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        index.index_entity("docs", &entity);
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);
        assert!(
            index_live.saturating_add(peak) as u64 <= already_charged.saturating_add(bound),
            "id={id} live_before={index_live} peak_growth={peak} charged={already_charged} reservation={bound}"
        );
    }
    let live = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    let reported = index.memory_bytes();
    eprintln!("context resident allocations={live}, conservative accounting={reported}");
    assert!(live > 1024 * 1024, "exercise a substantial resident index");
    assert!(live as u64 <= reported, "live={live} reported={reported}");
    for id in 1..=999 {
        index.remove_entity(EntityId::new(id));
    }
    let retained = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    assert!(
        retained as u64 <= index.memory_bytes(),
        "retained capacity must stay charged"
    );
    index.remove_entity(EntityId::new(1000));
    let drained = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    assert!(
        drained as u64 <= index.memory_bytes(),
        "empty maps still retain allocations"
    );
}
