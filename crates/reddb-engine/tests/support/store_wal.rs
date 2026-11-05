//! Canonical logical WAL framing and resident replay, shared with browser Workers.
use reddb_engine::storage::{
    entity::{CrossRef, EntityId, RefType, UnifiedEntity},
    manager::ManagerConfig,
    metadata_codec::serialize_entity_record,
    resident_store::ResidentStore,
    store_wal::StoreWalAction,
};
use reddb_file::{decode_store_wal_action_frame, StoreWalActionFrame};
use reddb_types::Value;
use std::cell::Cell;
use std::collections::HashMap;

pub const CASES: &[(&str, fn())] = &[
    ("store_wal_canonical_action_frames", canonical_action_frames),
    (
        "store_wal_impossible_counts_and_truncation",
        impossible_counts_and_truncation,
    ),
    ("store_wal_entity_record_replay", entity_record_replay),
    ("store_wal_host_index_boundaries", host_index_boundaries),
    ("store_wal_atomic_resident_refresh", atomic_resident_refresh),
    ("store_wal_aux_metadata_replay", aux_metadata_replay),
    ("store_wal_recovery_fixture", recovery_fixture),
];

pub fn recovery_actions() -> Vec<StoreWalAction> {
    let metadata = super::metadata_corpus::metadata();
    let mut actions = vec![StoreWalAction::CreateCollection {
        name: "empty".into(),
    }];
    for entity in super::record_corpus::entities() {
        let collection = format!("record_{}", entity.id.raw());
        let upsert = StoreWalAction::upsert_entity(&collection, &entity, Some(&metadata), 12);
        actions.push(upsert.clone());
        actions.push(StoreWalAction::DeleteEntityRecord {
            collection,
            entity_id: entity.id.raw(),
        });
        actions.push(upsert);
    }
    actions
}

pub fn verify_recovery(store: &ResidentStore) -> u32 {
    let entities = super::record_corpus::entities();
    let metadata = super::metadata_corpus::metadata();
    assert_eq!(store.list_collections().len(), entities.len() + 1);
    assert_eq!(
        store
            .get_collection("empty")
            .expect("empty collection")
            .count(),
        0
    );
    assert_eq!(store.query_all(|_| true).len(), entities.len());
    for entity in &entities {
        let collection = format!("record_{}", entity.id.raw());
        let manager = store
            .get_collection(&collection)
            .expect("replayed collection");
        let (owner, found) = store.get_any(entity.id).expect("replayed entity");
        assert_eq!(owner, collection);
        assert_eq!(manager.count(), 1);
        assert_eq!(
            serialize_entity_record(&found, manager.get_metadata(entity.id).as_ref(), 12),
            serialize_entity_record(entity, Some(&metadata), 12)
        );
    }
    u32::try_from(entities.len()).expect("fixture count fits u32")
}

fn recovery_fixture() {
    let store = ResidentStore::new();
    for action in recovery_actions() {
        replay(&store, action);
    }
    verify_recovery(&store);
}

fn actions() -> Vec<StoreWalAction> {
    vec![
        StoreWalAction::CreateCollection {
            name: "ação".into(),
        },
        StoreWalAction::DropCollection { name: "old".into() },
        StoreWalAction::UpsertEntityRecord {
            collection: "data".into(),
            record: vec![1, 2, 3],
        },
        StoreWalAction::DeleteEntityRecord {
            collection: "data".into(),
            entity_id: u64::MAX,
        },
        StoreWalAction::BulkUpsertEntityRecords {
            collection: "data".into(),
            records: vec![vec![], vec![4]],
        },
        StoreWalAction::RefreshCollection {
            collection: "data".into(),
            records: vec![vec![5], vec![6]],
        },
        StoreWalAction::SetAuxMetadata {
            bytes: vec![0, 1, 255],
        },
    ]
}

fn canonical_action_frames() {
    let expected = [
        StoreWalActionFrame::CreateCollection {
            name: "ação".into(),
        },
        StoreWalActionFrame::DropCollection { name: "old".into() },
        StoreWalActionFrame::UpsertEntityRecord {
            collection: "data".into(),
            record: vec![1, 2, 3],
        },
        StoreWalActionFrame::DeleteEntityRecord {
            collection: "data".into(),
            entity_id: u64::MAX,
        },
        StoreWalActionFrame::BulkUpsertEntityRecords {
            collection: "data".into(),
            records: vec![vec![], vec![4]],
        },
        StoreWalActionFrame::RefreshCollection {
            collection: "data".into(),
            records: vec![vec![5], vec![6]],
        },
        StoreWalActionFrame::SetAuxMetadata {
            bytes: vec![0, 1, 255],
        },
    ];
    for (action, expected) in actions().into_iter().zip(expected) {
        let bytes = action.encode();
        assert_eq!(
            decode_store_wal_action_frame(&bytes).expect("canonical frame"),
            expected
        );
        assert_eq!(
            StoreWalAction::decode(&bytes)
                .expect("typed decode")
                .encode(),
            bytes
        );
    }
}

fn impossible_counts_and_truncation() {
    for action in actions() {
        let bytes = action.encode();
        for end in 0..bytes.len() {
            assert!(
                StoreWalAction::decode(&bytes[..end]).is_err(),
                "incomplete action at {end}"
            );
        }
    }
    for tag in [5, 6] {
        let mut bytes = vec![reddb_file::STORE_WAL_ACTION_VERSION, tag];
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // Empty collection name.
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            StoreWalAction::decode(&bytes)
                .expect_err("bounded count")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}

fn replay(store: &ResidentStore, action: StoreWalAction) {
    let action = StoreWalAction::decode(&action.encode()).expect("persisted action");
    store
        .apply_replayed_action(&action, 12, &ManagerConfig::default())
        .expect("resident replay");
}

fn aux_metadata_replay() {
    let store = ResidentStore::new();
    for bytes in [vec![0, 1, 255], vec![0, 1, 255], vec![]] {
        replay(
            &store,
            StoreWalAction::SetAuxMetadata {
                bytes: bytes.clone(),
            },
        );
        assert_eq!(store.aux_metadata(), bytes);
    }
}

fn entity_record_replay() {
    let store = ResidentStore::new();
    let metadata = super::metadata_corpus::metadata();
    let entities = super::record_corpus::entities();
    for entity in &entities {
        let collection = format!("record_{}", entity.id.raw());
        replay(
            &store,
            StoreWalAction::upsert_entity(&collection, entity, Some(&metadata), 12),
        );
        let manager = store
            .get_collection(&collection)
            .expect("recovered collection");
        let found = manager.get(entity.id).expect("recovered entity");
        assert_eq!(
            serialize_entity_record(&found, manager.get_metadata(entity.id).as_ref(), 12),
            serialize_entity_record(entity, Some(&metadata), 12)
        );
        assert!(store.get_any(entity.id).is_some());
    }
    for entity in &entities {
        let collection = format!("record_{}", entity.id.raw());
        replay(
            &store,
            StoreWalAction::DeleteEntityRecord {
                collection: collection.clone(),
                entity_id: entity.id.raw(),
            },
        );
        assert!(
            store.get_any(entity.id).is_none(),
            "replay invalidates the entity cache"
        );
        replay(
            &store,
            StoreWalAction::DeleteEntityRecord {
                collection,
                entity_id: entity.id.raw(),
            },
        );
    }
    assert!(store.query_all(|_| true).is_empty());
    replay(
        &store,
        StoreWalAction::CreateCollection {
            name: "empty".into(),
        },
    );
    replay(
        &store,
        StoreWalAction::CreateCollection {
            name: "empty".into(),
        },
    );
    assert!(store.get_collection("empty").is_some());
    replay(
        &store,
        StoreWalAction::DropCollection {
            name: "empty".into(),
        },
    );
    replay(
        &store,
        StoreWalAction::DropCollection {
            name: "empty".into(),
        },
    );
    assert!(store.get_collection("empty").is_none());
}

fn host_index_boundaries() {
    let store = ResidentStore::new();
    let config = ManagerConfig::default();
    store.context_index().set_collection_enabled("nodes", true);
    let mut node = UnifiedEntity::graph_node(
        EntityId::new(1),
        "Person",
        "person",
        HashMap::from([("name".into(), Value::text("alice"))]),
    );
    node.add_cross_ref(CrossRef::new(
        node.id,
        EntityId::new(9),
        "nodes",
        RefType::RowToNode,
    ));
    let record = serialize_entity_record(&node, None, 12);
    store
        .apply_replayed_upsert("nodes", &record, 12, &config)
        .expect("node");
    assert!(store.get_any(node.id).is_some());
    let called = Cell::new(false);
    store
        .apply_replayed_delete("nodes", node.id, || {
            called.set(true);
            // The native pager hook runs outside resident locks, after deleting the
            // entity and evicting its cached copy, before derived-index cleanup.
            assert!(store.get_any(node.id).is_none());
            assert_eq!(store.get_refs_from(node.id).len(), 1);
            assert_eq!(store.lookup_graph_nodes_by_label("Person"), [node.id]);
            assert_eq!(store.context_index().search("alice", 10, None).len(), 1);
        })
        .expect("delete");
    assert!(called.get());
    assert!(store.get_refs_from(node.id).is_empty());
    assert!(store.lookup_graph_nodes_by_label("Person").is_empty());
    assert!(store.context_index().search("alice", 10, None).is_empty());
    store
        .apply_replayed_upsert("nodes", &record, 12, &config)
        .expect("restore node");
    assert!(store.get_any(node.id).is_some());
    called.set(false);
    store
        .drop_collection_in_memory("nodes", || {
            called.set(true);
            assert!(store.get_collection("nodes").is_none());
            assert!(store.get_refs_from(node.id).is_empty());
            assert_eq!(
                store.entity_cache_stats().entries,
                1,
                "native cache cleanup follows pager retirement"
            );
        })
        .expect("drop");
    assert!(called.get());
    assert_eq!(store.entity_cache_stats().entries, 0);
    assert!(store.lookup_graph_nodes_by_label("Person").is_empty());
    store
        .drop_collection_in_memory("nodes", || panic!("missing collection has no retirement"))
        .expect("idempotent drop");
}

fn atomic_resident_refresh() {
    let store = ResidentStore::new();
    let row = |id| {
        UnifiedEntity::table_row(
            EntityId::new(id),
            "view",
            id,
            vec![Value::Integer(id as i64)],
        )
    };
    let records = |ids: &[u64]| {
        ids.iter()
            .map(|id| serialize_entity_record(&row(*id), None, 12))
            .collect()
    };
    replay(
        &store,
        StoreWalAction::BulkUpsertEntityRecords {
            collection: "view".into(),
            records: records(&[1, 2]),
        },
    );
    let before = store.get_collection("view").expect("old reader");
    assert!(store.get_any(EntityId::new(1)).is_some());
    replay(
        &store,
        StoreWalAction::RefreshCollection {
            collection: "view".into(),
            records: records(&[3]),
        },
    );
    assert_eq!(before.count(), 2, "readers retain the prior manager");
    let after = store.get_collection("view").expect("new reader");
    assert_eq!(after.count(), 1);
    assert_eq!(after.query_all(|_| true)[0].id.raw(), 3);
    assert!(store.get_any(EntityId::new(1)).is_none());
    assert_eq!(store.next_entity_id().raw(), 4);
    replay(
        &store,
        StoreWalAction::RefreshCollection {
            collection: "view".into(),
            records: vec![],
        },
    );
    assert_eq!(
        store
            .get_collection("view")
            .expect("empty refreshed collection")
            .count(),
        0
    );
    assert_eq!(after.count(), 1, "the previous generation remains intact");
}
