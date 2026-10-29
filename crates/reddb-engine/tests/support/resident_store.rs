//! Canonical multi-collection resident reads, independent of durable host writes.
use reddb_engine::storage::{
    entity::{EntityId, RefType, UnifiedEntity},
    error::StoreError,
    manager::{ManagerConfig, SegmentManager},
    metadata::{Metadata, MetadataFilter, MetadataValue},
    resident_store::ResidentStore,
};
use reddb_engine::transaction::{
    read::SnapshotContext,
    snapshot::{Snapshot, SnapshotManager},
};
use reddb_types::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CASES: &[(&str, fn())] = &[
    ("resident_store_global_identity", global_identity),
    (
        "resident_store_reference_write_boundaries",
        reference_write_boundaries,
    ),
    (
        "resident_store_cross_collection_snapshot",
        cross_collection_snapshot,
    ),
    ("resident_store_models_and_cache", models_and_cache),
    (
        "resident_store_cache_reads_current_headers_and_payloads",
        cache_reads_current_headers_and_payloads,
    ),
    (
        "resident_store_graph_metadata_and_references",
        graph_metadata_and_references,
    ),
];

fn cache_reads_current_headers_and_payloads() {
    let store = ResidentStore::new();
    let manager = collection(&store, "cached");
    let id = EntityId::new(1);
    manager
        .insert(UnifiedEntity::table_row(id, "cached", 1, vec![]))
        .expect("row");
    manager.force_seal().expect("sealed row");
    assert_eq!(store.get_any(id).expect("warm cache").1.xmax, 0);
    manager
        .compare_exchange_xmax(id, 0, 9)
        .expect("delete header")
        .expect("matched");
    assert_eq!(
        store.get_any(id).expect("current header").1.xmax,
        9,
        "a cached pre-delete image must never escape after a completed stamp"
    );
    manager
        .compare_exchange_xmax(id, 9, 0)
        .expect("rollback")
        .expect("matched");
    assert_eq!(store.get_any(id).expect("revived header").1.xmax, 0);
    let mut replacement = manager.get(id).expect("row");
    replacement.data = UnifiedEntity::table_row(id, "cached", 1, vec![Value::Integer(42)]).data;
    manager
        .update(replacement.clone())
        .expect("new authoritative payload");
    assert_eq!(
        store
            .get_any(id)
            .expect("updated payload")
            .1
            .data
            .as_row()
            .expect("row")
            .columns,
        vec![Value::Integer(42)],
        "a cache hit must hydrate current storage"
    );
    manager.delete(id).expect("physical reclamation");
    store.entity_cache.insert(id.raw(), "cached".into());
    assert!(
        store.get_any(id).is_none(),
        "a stale cached row must not survive reclamation"
    );
}

fn collection(store: &ResidentStore, name: &str) -> Arc<SegmentManager> {
    Arc::clone(
        store
            .collections
            .write()
            .entry(name.into())
            .or_insert_with(|| Arc::new(SegmentManager::new(name))),
    )
}

fn global_identity() {
    let store = ResidentStore::new();
    assert_eq!(store.next_entity_id().raw(), 1);
    assert_eq!(store.reserve_entity_ids(3), 2..5);
    store.register_entity_id(EntityId::new(9_007_199_254_740_993));
    assert_eq!(store.next_entity_id().raw(), 9_007_199_254_740_994);
    store.register_entity_id(EntityId::new(7));
    assert_eq!(store.next_entity_id().raw(), 9_007_199_254_740_995);
    let empty = store.reserve_entity_ids(0);
    assert!(empty.is_empty());
    assert_eq!(store.next_entity_id().raw(), empty.start);
}

fn cross_collection_snapshot() {
    let store = ResidentStore::new();
    for (name, offset) in [("a", 100), ("b", 200)] {
        let manager = collection(&store, name);
        for (i, (xmin, xmax)) in [(0, 0), (3, 12), (12, 0), (3, 4)].into_iter().enumerate() {
            let id = offset + i as u64 + 1;
            let mut entity = UnifiedEntity::table_row(EntityId::new(id), name, id, vec![]);
            entity.xmin = xmin;
            entity.xmax = xmax;
            entity.set_logical_id(EntityId::new(offset));
            store.register_entity_id(entity.id);
            manager.insert(entity).expect("physical version");
            if i == 1 {
                manager.force_seal().expect("seal history");
            }
        }
    }
    let snapshot = SnapshotContext {
        snapshot: Snapshot {
            xid: 10,
            in_progress: HashSet::from([4]),
        },
        manager: Arc::new(SnapshotManager::new()),
        own_xids: HashSet::new(),
        requires_index_fallback: true,
        serializable_reader: None,
    };
    let ids = |rows: Vec<(String, UnifiedEntity)>| {
        let mut ids: Vec<_> = rows
            .into_iter()
            .map(|(name, entity)| (name, entity.id.raw()))
            .collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(
        ids(store.scan(Some(&snapshot), |_| true)),
        vec![
            ("a".into(), 101),
            ("a".into(), 102),
            ("a".into(), 104),
            ("b".into(), 201),
            ("b".into(), 202),
            ("b".into(), 204),
        ]
    );
    assert_eq!(
        ids(store.scan(None, |_| true)),
        vec![
            ("a".into(), 101),
            ("a".into(), 103),
            ("b".into(), 201),
            ("b".into(), 203),
        ]
    );
    assert_eq!(
        store.query_all(|_| true).len(),
        8,
        "maintenance reads retain every physical version"
    );
    assert_eq!(
        store
            .table_row_versions_by_logical_id("a", EntityId::new(100))
            .len(),
        4
    );
    assert!(store
        .table_row_versions_by_logical_id("missing", EntityId::new(100))
        .is_empty());
}

fn models_and_cache() {
    let store = ResidentStore::new();
    let entities = crate::record_corpus::entities();
    for entity in &entities {
        store.register_entity_id(entity.id);
        collection(&store, entity.kind.collection())
            .insert(entity.clone())
            .expect("native shape");
    }
    assert_eq!(store.entity_cache_hit_rate(), None);
    for entity in &entities {
        let (name, found) = store.get_any(entity.id).expect("canonical resident read");
        assert_eq!(name, entity.kind.collection());
        assert_eq!(found.kind, entity.kind);
        assert_eq!(found.logical_id(), entity.logical_id());
    }
    let first = &entities[0];
    assert!(store.get_any(first.id).is_some());
    assert_eq!(store.entity_cache_stats().entries, entities.len());
    assert_eq!(store.entity_cache_stats().hits, 1);
    assert!(store.get_any(EntityId::new(u64::MAX)).is_none());
    assert!(store.get_collection("missing").is_none());
    assert_eq!(store.query_all(|_| true).len(), entities.len());
}

fn graph_metadata_and_references() {
    let store = ResidentStore::new();
    for (name, id) in [("a", 1), ("b", 2)] {
        let manager = collection(&store, name);
        manager
            .insert(UnifiedEntity::graph_node(
                EntityId::new(id),
                "Person",
                "person",
                HashMap::new(),
            ))
            .expect("node");
        store.update_graph_label_index(name, "Person", EntityId::new(id));
        manager
            .set_metadata(
                EntityId::new(id),
                Metadata::with_fields(HashMap::from([(
                    "source".into(),
                    MetadataValue::String("offline".into()),
                )])),
            )
            .expect("metadata");
    }
    assert_eq!(
        store.lookup_graph_nodes_by_label_in("a", "Person"),
        [EntityId::new(1)]
    );
    let mut all = store.lookup_graph_nodes_by_label("Person");
    all.sort_unstable();
    assert_eq!(all, [EntityId::new(1), EntityId::new(2)]);
    store.remove_from_graph_label_index_batch("a", &[EntityId::new(1)]);
    assert_eq!(
        store.lookup_graph_nodes_by_label("Person"),
        [EntityId::new(2)]
    );
    store.remove_from_graph_label_index("b", EntityId::new(2));
    assert!(store.lookup_graph_nodes_by_label("Person").is_empty());
    let mut matched = store.filter_metadata_all(&[(
        "source".into(),
        MetadataFilter::Eq(MetadataValue::String("offline".into())),
    )]);
    matched.sort_unstable();
    assert_eq!(
        matched,
        [
            ("a".into(), EntityId::new(1)),
            ("b".into(), EntityId::new(2))
        ]
    );
    store.cross_refs.write().insert(
        EntityId::new(1),
        vec![(EntityId::new(2), RefType::RowToNode, "b".into())],
    );
    store.reverse_refs.write().insert(
        EntityId::new(2),
        vec![(EntityId::new(1), RefType::RowToNode, "a".into())],
    );
    assert_eq!(
        store.get_refs_from(EntityId::new(1)),
        [(EntityId::new(2), RefType::RowToNode, "b".into())]
    );
    assert_eq!(
        store.get_refs_to(EntityId::new(2)),
        [(EntityId::new(1), RefType::RowToNode, "a".into())]
    );
    store.set_aux_metadata(b"opaque catalog".to_vec());
    assert_eq!(store.aux_metadata(), b"opaque catalog");
}

fn reference_write_boundaries() {
    use std::cell::Cell;
    use std::sync::atomic::Ordering;
    let store = ResidentStore::new();
    let config = ManagerConfig::default();
    for (collection, id) in [("source", 1), ("target", 2)] {
        store
            .get_or_create_collection(collection, &config)
            .restore_snapshot_entity(UnifiedEntity::table_row(
                EntityId::new(id),
                collection,
                id,
                vec![],
            ))
            .expect("reference endpoint");
    }
    store.paged_registry_dirty.store(false, Ordering::Release);
    let called = Cell::new(false);
    assert!(store
        .add_cross_ref_in_memory(
            "source",
            EntityId::new(1),
            "target",
            EntityId::new(2),
            RefType::RowToNode,
            0.5,
            1000,
            |entity| {
                called.set(true);
                assert_eq!(entity.cross_refs().len(), 1);
                assert_eq!(store.get_refs_from(entity.id).len(), 1);
                assert_eq!(store.get_refs_to(EntityId::new(2)).len(), 1);
                assert!(
                    !store.paged_registry_dirty.load(Ordering::Acquire),
                    "native pager hook precedes registry dirty publication"
                );
                Ok(())
            }
        )
        .expect("add reference"));
    assert!(called.get());
    assert!(store.paged_registry_dirty.load(Ordering::Acquire));
    store.paged_registry_dirty.store(false, Ordering::Release);
    assert!(!store
        .add_cross_ref_in_memory(
            "source",
            EntityId::new(1),
            "target",
            EntityId::new(2),
            RefType::RowToNode,
            0.5,
            1000,
            |_| panic!("duplicate reference has no pager write")
        )
        .expect("duplicate"));
    assert!(!store.paged_registry_dirty.load(Ordering::Acquire));
    assert!(matches!(
        store.add_cross_ref_in_memory(
            "source",
            EntityId::new(1),
            "target",
            EntityId::new(2),
            RefType::RowToNode,
            0.5,
            1,
            |_| panic!("limit precedes mutation")
        ),
        Err(StoreError::TooManyRefs(_))
    ));
    assert!(matches!(
        store.add_cross_ref_in_memory(
            "missing",
            EntityId::new(1),
            "target",
            EntityId::new(2),
            RefType::RowToNode,
            0.5,
            1000,
            |_| panic!("missing endpoint")
        ),
        Err(StoreError::CollectionNotFound(_))
    ));
}
