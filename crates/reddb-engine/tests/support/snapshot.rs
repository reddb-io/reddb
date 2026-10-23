//! Native dump recovery cases also executed by the browser WASM harness.
use reddb_engine::storage::{
    entity::UnifiedEntity,
    metadata::Metadata,
    snapshot::{visit_snapshot, SnapshotVisitor},
};
use reddb_file::native_value::NativeReader;
use std::collections::BTreeMap;

pub const CASES: &[(&str, fn())] = &[
    ("snapshot_native_v1_v12_recovery", native_versions),
    (
        "snapshot_empty_collections_and_legacy_tails",
        optional_tails,
    ),
    ("snapshot_corruption_and_callback_errors", malformed),
];

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
        assert!(restored.collections["empty"].is_empty());
        let entities = &restored.collections["ação"];
        assert_eq!(entities.len(), 8);
        for (index, (entity, metadata)) in entities.iter().enumerate() {
            assert_eq!(entity.id.raw(), 1024 + index as u64);
            assert_eq!(entity.sequence_id, 99);
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
