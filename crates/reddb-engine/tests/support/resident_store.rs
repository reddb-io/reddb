//! Canonical multi-collection resident reads, independent of durable host writes.
use reddb_engine::storage::{
    entity::{EntityId, RefType, UnifiedEntity},
    manager::SegmentManager,
    metadata::{Metadata, MetadataFilter, MetadataValue},
    resident_store::ResidentStore,
};
use reddb_engine::transaction::{
    read::SnapshotContext,
    snapshot::{Snapshot, SnapshotManager},
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CASES: &[(&str, fn())] = &[
    ("resident_store_global_identity", global_identity),
    (
        "resident_store_cross_collection_snapshot",
        cross_collection_snapshot,
    ),
    ("resident_store_models_and_cache", models_and_cache),
    (
        "resident_store_graph_metadata_and_references",
        graph_metadata_and_references,
    ),
];

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
                name,
                "Person",
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
