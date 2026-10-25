//! Native dump recovery cases also executed by the browser WASM harness.
use reddb_engine::storage::{
    entity::{EntityId, UnifiedEntity},
    manager::ManagerConfig,
    metadata::Metadata,
    resident_store::ResidentStore,
    snapshot::{visit_snapshot, SnapshotVisitor},
};
use reddb_file::native_value::NativeReader;
use std::collections::BTreeMap;

pub const CASES: &[(&str, fn())] = &[
    ("snapshot_native_store_fixture", native_store_fixture),
    ("snapshot_native_v1_v12_recovery", native_versions),
    (
        "snapshot_empty_collections_and_legacy_tails",
        optional_tails,
    ),
    ("snapshot_corruption_and_callback_errors", malformed),
    (
        "snapshot_resident_recovery_options",
        resident_recovery_options,
    ),
    (
        "snapshot_native_committed_projection",
        native_committed_projection,
    ),
];

fn native_store_fixture() {
    verify_native_snapshot(include_bytes!("../fixtures/native-store-v12.rdst"));
}

/// Assert the same native-produced snapshot after an actual OPFS restart.
pub fn verify_native_snapshot(bytes: &[u8]) -> u32 {
    let mut restored = Restored::default();
    assert_eq!(
        visit_snapshot(bytes, &mut restored).expect("native snapshot"),
        12
    );
    assert_eq!(restored.collections.len(), 9);
    assert!(restored.collections["empty"].is_empty());
    assert_eq!(restored.auxiliary, b"opaque catalog");
    assert_eq!(restored.cross_refs.len(), 9);
    assert!(restored
        .cross_refs
        .iter()
        .any(|cross_ref| cross_ref.source_id == 1024 && cross_ref.target_id == 1026));
    let (resident, version) =
        ResidentStore::from_snapshot(bytes, &ManagerConfig::default(), true, 1000)
            .expect("canonical resident recovery");
    assert_eq!(version, 12);
    assert!(resident
        .get_refs_from(EntityId::new(1024))
        .iter()
        .any(|(id, _, _)| id.raw() == 1026));
    assert_eq!(resident.list_collections().len(), 9);
    assert_eq!(
        resident
            .get_collection("empty")
            .expect("empty collection")
            .count(),
        0
    );
    assert_eq!(resident.aux_metadata(), b"opaque catalog");
    for expected in super::codec_corpus::entities() {
        let collection = &restored.collections[&format!("codec_{}", expected.id.raw())];
        assert_eq!(collection.len(), 1);
        let (entity, metadata) = &collection[0];
        assert_eq!(entity.id, expected.id);
        assert_eq!(entity.kind, expected.kind);
        let manager = resident
            .get_collection(&format!("codec_{}", expected.id.raw()))
            .expect("recovered resident collection");
        let (owner, cached) = resident.get_any(entity.id).expect("recovered store lookup");
        assert_eq!(owner, format!("codec_{}", expected.id.raw()));
        assert_eq!(cached.kind, entity.kind);
        let managed = manager
            .get(entity.id)
            .expect("recovered entity remains discoverable");
        assert_eq!(
            manager.query_all(|_| true).len(),
            1,
            "physical history survives recovery"
        );
        assert_eq!(
            reddb_engine::storage::metadata_codec::serialize_entity_record(
                &managed,
                manager.get_metadata(entity.id).as_ref(),
                12,
            ),
            reddb_engine::storage::metadata_codec::serialize_entity_record(
                entity,
                metadata.as_ref(),
                12
            ),
            "recovery retains MVCC, sequence, payload and metadata bytes",
        );
        assert!(
            restored
                .cross_refs
                .iter()
                .any(|cross_ref| cross_ref.source_id == expected.id.raw()
                    && cross_ref.target_id == 77)
        );
        assert_eq!(
            metadata.as_ref().expect("typed metadata").fields,
            super::metadata_corpus::metadata().fields
        );
    }
    8
}

#[derive(Default)]
pub struct Restored {
    pub collections: BTreeMap<String, Vec<(UnifiedEntity, Option<Metadata>)>>,
    pub cross_refs: Vec<reddb_file::NativeDumpCrossRef>,
    pub auxiliary: Vec<u8>,
}

impl SnapshotVisitor for Restored {
    fn collection(&mut self, name: &str) -> Result<(), String> {
        self.collections.entry(name.into()).or_default();
        Ok(())
    }
    fn entity(
        &mut self,
        collection: &str,
        entity: UnifiedEntity,
        metadata: Option<Metadata>,
    ) -> Result<(), String> {
        self.collections
            .get_mut(collection)
            .expect("collection before entity")
            .push((entity, metadata));
        Ok(())
    }
    fn cross_ref(&mut self, cross_ref: reddb_file::NativeDumpCrossRef) -> Result<(), String> {
        self.cross_refs.push(cross_ref);
        Ok(())
    }
    fn auxiliary(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.auxiliary = bytes.to_vec();
        Ok(())
    }
}

fn dump(version: u32, tails: bool) -> Vec<u8> {
    let mut bytes = reddb_file::encode_native_store_header(version);
    reddb_file::encode_native_dump_count(&mut bytes, 2);
    reddb_file::encode_native_dump_collection_header(&mut bytes, "empty", 0);
    reddb_file::encode_native_dump_collection_header(&mut bytes, "ação", 8);
    // Frozen original entity bytes, rather than the reader's current encoder.
    let mut position = 0;
    let mut reader = NativeReader::new(
        include_bytes!("../fixtures/native-entity-v1-v12.bin"),
        &mut position,
    );
    while reader.remaining() > 0 {
        let entity_version = u32::from_le_bytes(reader.fixed().expect("version"));
        let _index = u32::from_le_bytes(reader.fixed().expect("index"));
        let size = u32::from_le_bytes(reader.fixed().expect("length")) as usize;
        let entity = reader.take(size).expect("frozen entity");
        if version != entity_version {
            continue;
        }
        if version >= 7 {
            let record = reddb_file::encode_native_entity_record_frame(
                entity,
                Some(include_bytes!("../fixtures/native-metadata.bin")),
            );
            reddb_file::encode_native_dump_entity_record(&mut bytes, &record);
        } else {
            bytes.extend_from_slice(entity);
        }
    }
    if tails {
        reddb_file::encode_native_dump_count(&mut bytes, 1);
        reddb_file::encode_native_dump_cross_ref(&mut bytes, 1024, 1026, 0, "ação");
        if version >= 10 {
            reddb_file::encode_native_dump_entity_record(&mut bytes, b"opaque catalog");
        }
    }
    if version >= 3 {
        reddb_file::append_native_store_crc32_footer(&mut bytes);
    }
    bytes
}

fn native_versions() {
    for version in 1..=reddb_file::STORE_VERSION_CURRENT {
        let mut restored = Restored::default();
        assert_eq!(
            visit_snapshot(&dump(version, true), &mut restored).expect("native snapshot"),
            version
        );
        let (resident, recovered_version) = ResidentStore::from_snapshot(
            &dump(version, true),
            &ManagerConfig::default(),
            true,
            1000,
        )
        .expect("canonical resident version recovery");
        assert_eq!(recovered_version, version);
        assert_eq!(resident.list_collections().len(), 2);
        assert_eq!(resident.get_collection("empty").expect("empty").count(), 0);
        assert_eq!(resident.aux_metadata(), restored.auxiliary);
        let manager = resident
            .get_collection("ação")
            .expect("recovered collection");
        assert_eq!(manager.count(), 8);
        let entities = &restored.collections["ação"];
        assert_eq!(entities.len(), 8);
        for (index, (entity, metadata)) in entities.iter().enumerate() {
            assert_eq!(entity.id.raw(), 1024 + index as u64);
            assert_eq!(entity.sequence_id, 99);
            let managed = manager.get(entity.id).expect("canonical resident entity");
            assert_eq!(managed.sequence_id, entity.sequence_id);
            assert_eq!(managed.xmin, entity.xmin);
            assert_eq!(managed.xmax, entity.xmax);
            assert_eq!(
                manager
                    .get_metadata(entity.id)
                    .expect("live legacy records expose default metadata")
                    .fields,
                metadata.clone().unwrap_or_default().fields
            );
            assert_eq!(entity.xmin, if version >= 9 { 17 } else { 0 });
            assert_eq!(entity.xmax, if version >= 9 { 23 } else { 0 });
            assert_eq!(
                metadata.as_ref().map(|m| &m.fields),
                (version >= 7)
                    .then(super::metadata_corpus::metadata)
                    .as_ref()
                    .map(|m| &m.fields)
            );
        }
        assert_eq!(restored.cross_refs.len(), 1);
        assert_eq!(restored.cross_refs[0].source_id, 1024);
        assert_eq!(restored.cross_refs[0].target_id, 1026);
        assert_eq!(
            restored.auxiliary,
            if version >= 10 {
                b"opaque catalog".as_slice()
            } else {
                &[]
            }
        );
    }
}

fn optional_tails() {
    for version in 1..=reddb_file::STORE_VERSION_CURRENT {
        let mut restored = Restored::default();
        visit_snapshot(&dump(version, false), &mut restored)
            .expect("legacy missing optional tails");
        assert_eq!(restored.collections.len(), 2);
        assert!(restored.cross_refs.is_empty());
        assert!(restored.auxiliary.is_empty());
    }
}

fn malformed() {
    let bytes = dump(12, true);
    for end in 0..bytes.len() {
        let mut restored = Restored::default();
        assert!(visit_snapshot(&bytes[..end], &mut restored).is_err());
        assert!(
            restored.collections.is_empty(),
            "CRC verified before callbacks"
        );
    }
    let mut corrupt = bytes.clone();
    corrupt[20] ^= 1;
    assert!(visit_snapshot(&corrupt, &mut Restored::default()).is_err());
    let mut impossible = reddb_file::encode_native_store_header(12);
    reddb_file::encode_native_dump_count(&mut impossible, u32::MAX);
    reddb_file::append_native_store_crc32_footer(&mut impossible);
    assert!(visit_snapshot(&impossible, &mut Restored::default()).is_err());

    struct Rejected;
    impl SnapshotVisitor for Rejected {
        fn collection(&mut self, _: &str) -> Result<(), String> {
            Err("host refused collection".into())
        }
        fn entity(&mut self, _: &str, _: UnifiedEntity, _: Option<Metadata>) -> Result<(), String> {
            panic!("after host rejection")
        }
        fn cross_ref(&mut self, _: reddb_file::NativeDumpCrossRef) -> Result<(), String> {
            panic!("after host rejection")
        }
        fn auxiliary(&mut self, _: &[u8]) -> Result<(), String> {
            panic!("after host rejection")
        }
    }
    assert_eq!(
        visit_snapshot(&bytes, &mut Rejected),
        Err("host refused collection".into())
    );
}

fn resident_recovery_options() {
    let config = ManagerConfig::default();
    for auto_index_refs in [false, true] {
        let (store, version) =
            ResidentStore::from_snapshot(&dump(12, false), &config, auto_index_refs, 1000)
                .expect("canonical reference indexing policy");
        assert_eq!(version, 12);
        assert_eq!(
            store.get_refs_from(EntityId::new(1024)).is_empty(),
            !auto_index_refs
        );
        assert_eq!(store.next_entity_id().raw(), 1032);
        assert!(store
            .get_collection("ação")
            .expect("recovered")
            .get(EntityId::new(1024))
            .is_some());
    }
    let (store, _) = ResidentStore::from_snapshot(&dump(12, true), &config, false, 0)
        .expect("native explicit-reference cap policy");
    assert!(store.get_refs_from(EntityId::new(1024)).is_empty());
    let (store, _) = ResidentStore::from_snapshot(&dump(12, true), &config, false, 1000)
        .expect("native explicit-reference restoration");
    assert!(store
        .get_refs_from(EntityId::new(1024))
        .iter()
        .any(|(id, _, _)| id.raw() == 1026));
    let bytes = dump(12, true);
    for end in [0, 8, bytes.len() - 1] {
        assert!(
            ResidentStore::from_snapshot(&bytes[..end], &config, true, 1000).is_err(),
            "failed recovery returns no resident target"
        );
    }
}

fn native_committed_projection() {
    use reddb_engine::storage::{
        entity::{EntityData, EntityKind},
        store_wal::StoreWalAction,
    };
    use std::cell::Cell;
    use std::collections::{HashMap, HashSet};
    let config = ManagerConfig::default();
    let store = ResidentStore::new();
    let mut old = UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![]);
    old.ensure_table_logical_id();
    old.xmax = 30;
    let mut active = UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![]);
    active.xmin = 30;
    let mut graph = UnifiedEntity::graph_node(EntityId::new(3), "kept", "node", HashMap::new());
    graph.xmin = 30;
    let pending = UnifiedEntity::graph_node(EntityId::new(4), "pending", "node", HashMap::new());
    let mut vector = UnifiedEntity::vector(EntityId::new(5), "vectors", vec![1.0]);
    vector.xmin = 30;
    for (collection, entity) in [
        ("rows", old),
        ("rows", active),
        ("graph", graph),
        ("graph", pending),
        ("vectors", vector),
    ] {
        store
            .apply_replayed_action(
                &StoreWalAction::upsert_entity(collection, &entity, None, 12),
                12,
                &config,
            )
            .expect("projection seed");
    }
    let called = Cell::new(false);
    let bytes = store.snapshot_bytes(
        &HashSet::from([EntityId::new(4)]),
        &HashSet::from([30]),
        || {
            called.set(true);
            assert!(
                store.collections.try_write().is_none(),
                "native format hook retains collection guard"
            );
            assert!(
                store.cross_refs.try_write().is_none(),
                "native format hook retains reference guard"
            );
        },
    );
    assert!(called.get());
    let mut restored = Restored::default();
    assert_eq!(
        visit_snapshot(&bytes, &mut restored).expect("canonical emitted snapshot"),
        12
    );
    let rows = &restored.collections["rows"];
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.id.raw(), 1);
    assert_eq!(
        rows[0].0.xmax, 0,
        "unfinished deletion is not published as committed"
    );
    assert!(matches!(rows[0].0.data, EntityData::Row(_)));
    assert_eq!(restored.collections["graph"].len(), 1);
    assert!(matches!(
        restored.collections["graph"][0].0.kind,
        EntityKind::GraphNode(_)
    ));
    assert!(restored.collections["vectors"].is_empty());
    let (resident, _) = ResidentStore::from_snapshot(
        include_bytes!("../fixtures/native-store-v12.rdst"),
        &config,
        true,
        1000,
    )
    .expect("frozen native source");
    verify_native_snapshot(&resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {}));
}
