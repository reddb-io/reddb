//! Compare the actual restore path's allocations with its budget observation.
use reddb_engine::storage::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb_engine::storage::manager::{ManagerConfig, SegmentManager};
use reddb_engine::storage::metadata::{Metadata, MetadataStorage, MetadataValue, RefTarget};
use reddb_engine::storage::resident_store::ResidentStore;
use reddb_engine::storage::segment::GrowingSegment;
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

#[derive(Clone, Copy, Debug)]
enum Fixture {
    PositionalIntegers,
    PositionalText,
    NamedText,
    RowMetadata,
}

fn restored_entity(id: u64, fixture: Fixture) -> UnifiedEntity {
    let data = match fixture {
        Fixture::PositionalIntegers | Fixture::RowMetadata => {
            RowData::new(vec![Value::Integer(id as i64), Value::Integer(7)])
        }
        Fixture::PositionalText => RowData::new(
            (0..12)
                .map(|field| Value::text(format!("record{id} field{field} resident payload")))
                .collect(),
        ),
        Fixture::NamedText => RowData {
            columns: Vec::new(),
            named: Some(
                (0..12)
                    .map(|field| {
                        (
                            format!("field{field}"),
                            Value::text(format!("record{id} field{field} resident payload")),
                        )
                    })
                    .collect(),
            ),
            schema: None,
        },
    };
    UnifiedEntity::new(
        EntityId::new(id),
        EntityKind::TableRow {
            table: "memory_probe".into(),
            row_id: id,
        },
        EntityData::Row(data),
    )
}

fn restored_memory(rows: u64, fixture: Fixture) -> (usize, usize) {
    let baseline = LIVE.load(Ordering::Relaxed);
    let entities: Vec<_> = (1..=rows).map(|id| restored_entity(id, fixture)).collect();
    let source_actual = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    let source_reported: usize = entities
        .iter()
        .map(reddb_engine::storage::memory_size::entity_bytes)
        .sum();
    let manager = SegmentManager::new("memory_probe");
    for entity in entities {
        let id = entity.id;
        manager
            .restore_snapshot_entity(entity)
            .expect("restore the same physical entity path used during native open");
        if matches!(fixture, Fixture::RowMetadata) {
            let mut metadata = Metadata::new();
            metadata.set("payload", MetadataValue::String("m".repeat(1024)));
            manager
                .set_metadata(id, metadata)
                .expect("restore physical per-entity metadata");
        }
    }
    for id in [1, rows] {
        assert_eq!(
            manager.get(EntityId::new(id)).expect("restored content").id,
            EntityId::new(id)
        );
    }
    let reported = usize::try_from(manager.resident_bytes())
        .expect("small fixture fits the target")
        + manager.checkpoint_tracking_bytes();
    let actual = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    drop(manager);
    eprintln!("source fixture={fixture:?} rows={rows} actual={source_actual} estimated={source_reported} auxiliary_allocation_delta={}", actual.saturating_sub(source_actual));
    (actual, reported)
}

#[test]
fn restored_segment_accounting_covers_allocation_growth() {
    eprintln!("restored segment requested-allocation observer");
    let mut failures = Vec::new();
    for fixture in [
        Fixture::PositionalIntegers,
        Fixture::PositionalText,
        Fixture::NamedText,
        Fixture::RowMetadata,
    ] {
        let small = restored_memory(1000, fixture);
        let large = restored_memory(4000, fixture);
        let actual_growth = large.0.saturating_sub(small.0);
        let reported_growth = large.1.saturating_sub(small.1);
        eprintln!("fixture={fixture:?} small_actual={} small_reported={} large_actual={} large_reported={} actual_growth={actual_growth} reported_growth={reported_growth}", small.0, small.1, large.0, large.1);
        // Fixed manager/harness costs cannot explain a gap proportional to rows.
        if actual_growth > reported_growth.saturating_add(64 * 1024) {
            failures.push((fixture, actual_growth, reported_growth));
        }
    }
    assert!(
        failures.is_empty(),
        "restore allocations grow outside the budget: {failures:?}"
    );
    metadata_replacement_and_removal_stay_accounted();
    deleted_segment_capacity_stays_accounted();
    consolidation_allocation_reclamation();
    header_stamps_keep_requested_allocations_bounded();
    cached_locations_do_not_retain_payload_copies();
}

fn cached_locations_do_not_retain_payload_copies() {
    for dimensions in [16, 65_536] {
        let store = ResidentStore::new();
        let manager = std::sync::Arc::new(SegmentManager::new("memory_probe"));
        for raw in 1..=8 {
            manager
                .insert(UnifiedEntity::vector(
                    EntityId::new(raw),
                    "memory_probe",
                    vec![1.0; dimensions],
                ))
                .expect("source vector");
        }
        manager.force_seal().expect("sealed source");
        store
            .collections
            .write()
            .insert("memory_probe".into(), manager);
        let before = LIVE.load(Ordering::Relaxed);
        for raw in 1..=8 {
            drop(store.get_any(EntityId::new(raw)).expect("warm location"));
        }
        let retained = LIVE.load(Ordering::Relaxed).saturating_sub(before);
        assert!(retained < 64 * 1024,
            "cached location ownership must not scale with payload: dimensions={dimensions}, retained={retained}");
        assert_eq!(store.entity_cache_stats().entries, 8);
        for raw in 1..=8 {
            drop(
                store
                    .get_any(EntityId::new(raw))
                    .expect("current source image"),
            );
        }
        assert_eq!(
            LIVE.load(Ordering::Relaxed),
            before + retained,
            "hits must not retain additional payload copies"
        );
        eprintln!("location cache dimensions={dimensions} retained={retained}");
    }
}

fn header_stamps_keep_requested_allocations_bounded() {
    for flat in [false, true] {
        let manager = SegmentManager::new("memory_probe");
        let rows = (1..=160).map(|id| restored_entity(id, Fixture::NamedText));
        if flat {
            manager.bulk_insert(rows.collect()).expect("flat source");
        } else {
            for row in rows {
                manager
                    .restore_snapshot_entity(row)
                    .expect("restored source");
            }
        }
        manager.force_seal().expect("sealed source");
        let reported = manager.resident_bytes();
        let journal = manager.checkpoint_tracking_bytes();
        let actual = LIVE.load(Ordering::Relaxed);
        PEAK.store(actual, Ordering::Relaxed);
        for generation in 1..=64 {
            for raw in 1..=160 {
                drop(
                    manager
                        .compare_exchange_xmax(EntityId::new(raw), generation - 1, generation)
                        .expect("stamp authoritative image")
                        .expect("matched previous header"),
                );
            }
            assert_eq!(
                LIVE.load(Ordering::Relaxed),
                actual,
                "equivalent header cycles must not retain new allocations"
            );
            assert_eq!(manager.resident_bytes(), reported);
            assert_eq!(manager.checkpoint_tracking_bytes(), journal);
        }
        let peak_extra = PEAK.load(Ordering::Relaxed).saturating_sub(actual);
        assert!(
            peak_extra <= 64 * 1024,
            "one returned WAL image must not clone the collection: {peak_extra}"
        );
        eprintln!("header stamps flat={flat} actual={actual} reported={reported} journal={journal} peak_extra={peak_extra}");
        assert_eq!(manager.stats().growing_count, 0);
        assert_eq!(manager.stats().total_entities, 160);
    }
}

fn metadata_replacement_and_removal_stay_accounted() {
    let baseline = LIVE.load(Ordering::Relaxed);
    let mut storage = MetadataStorage::new();
    for raw in 1..=400 {
        let id = EntityId::new(raw);
        for (name, value) in [
            ("bool", MetadataValue::Bool(true)),
            ("int", MetadataValue::Int(42)),
            ("float", MetadataValue::Float(1.25)),
            ("time", MetadataValue::Timestamp(17)),
            ("string", MetadataValue::String("s".repeat(1024))),
            ("bytes", MetadataValue::Bytes(vec![7; 1024])),
            ("null", MetadataValue::Null),
            (
                "nested",
                MetadataValue::Object(
                    [(
                        "array".to_string(),
                        MetadataValue::Array(vec![
                            MetadataValue::String("nested".repeat(40)),
                            MetadataValue::Reference(RefTarget::table("owned_table", raw)),
                            MetadataValue::References(vec![RefTarget::table("other_table", raw)]),
                        ]),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
        ] {
            storage.set(id, name, value);
        }
    }
    assert_metadata_accounted(&storage, baseline, "mixed metadata");
    // Type replacement must release the old value and preserve the entity's
    // key ownership; repeated replacements must not accumulate phantom usage.
    let mut stable_charge = None;
    for generation in 0..5 {
        for raw in 1..=400 {
            storage.set(
                EntityId::new(raw),
                "nested",
                MetadataValue::String("r".repeat(1024)),
            );
        }
        assert_metadata_accounted(&storage, baseline, "type replacement");
        if let Some(previous) = stable_charge {
            assert_eq!(
                storage.memory_bytes(),
                previous,
                "equivalent replacements retain one charge"
            );
        }
        stable_charge = Some(storage.memory_bytes());
        if generation == 0 {
            assert_eq!(
                storage.get(EntityId::new(1), "nested"),
                Some(MetadataValue::String("r".repeat(1024)))
            );
        }
    }
    for raw in 1..=400 {
        storage.remove_all(EntityId::new(raw));
        storage.remove_all(EntityId::new(raw));
    }
    assert_eq!(storage.entity_count(), 0);
    assert_eq!(storage.get(EntityId::new(1), "string"), None);
    assert_metadata_accounted(&storage, baseline, "empty retained capacity");
}

fn assert_metadata_accounted(storage: &MetadataStorage, baseline: usize, stage: &str) {
    let actual = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    let reported = storage.memory_bytes();
    eprintln!("metadata stage={stage} actual={actual} reported={reported}");
    assert!(
        actual <= reported.saturating_add(4096),
        "{stage}: actual={actual} reported={reported}"
    );
}

fn deleted_segment_capacity_stays_accounted() {
    let baseline = LIVE.load(Ordering::Relaxed);
    let mut segment = GrowingSegment::new(1, "memory_probe");
    for id in 1..=4000 {
        segment.adopt_entity(restored_entity(id, Fixture::PositionalIntegers), None);
    }
    for id in 1..=4000 {
        assert!(segment.force_delete(EntityId::new(id)));
    }
    let actual = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    let reported = usize::try_from(segment.resident_bytes())
        .expect("small fixture fits the target")
        + segment.checkpoint_tracking_bytes();
    eprintln!("deleted segment actual={actual} reported={reported}");
    assert!(
        actual <= reported.saturating_add(4096),
        "retained directories: actual={actual} reported={reported}"
    );
}

fn consolidation_allocation_reclamation() {
    let manager = SegmentManager::with_config(
        "memory_probe",
        ManagerConfig {
            max_sealed_segments: 1000,
            consolidation_entities_per_tick: 4,
            ..Default::default()
        },
    );
    let entities = (0..100)
        .map(|id| {
            UnifiedEntity::table_row(
                manager.next_entity_id(),
                "users",
                id,
                vec![Value::Integer(id as i64), Value::text(format!("row-{id}"))],
            )
        })
        .collect();
    let ids = manager.bulk_insert(entities).expect("flat source");
    manager.force_seal().expect("sealed source");
    for id in ids.iter().take(40) {
        manager.delete(*id).expect("tombstone");
    }
    let before_reported = manager.resident_bytes();
    let before_actual = LIVE.load(Ordering::Relaxed);
    for _ in 0..1000 {
        manager.run_maintenance().expect("paced maintenance");
        if manager.stats().consolidation.runs_completed != 0 {
            break;
        }
    }
    let after_actual = LIVE.load(Ordering::Relaxed);
    let after_reported = manager.resident_bytes();
    eprintln!("consolidation actual_before={before_actual} actual_after={after_actual} reported_before={before_reported} reported_after={after_reported}");
    assert_eq!(manager.stats().consolidation.runs_completed, 1);
    assert_eq!(manager.query_all(|_| true).len(), 60);
    assert!(after_actual < before_actual, "consolidation should reclaim real allocations: before={before_actual} after={after_actual}");
}
