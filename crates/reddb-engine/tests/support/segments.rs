//! The same resident storage assertions run natively and in browser workers.
use reddb_engine::storage::entity::{EntityData, EntityId, RowData, UnifiedEntity};
use reddb_engine::storage::metadata::{Metadata, MetadataValue};
use reddb_engine::storage::segment::{GrowingSegment, UnifiedSegment, SCAN_BATCH_SIZE};
use reddb_types::Value;
use std::collections::HashMap;

pub const CASES: &[(&str, fn())] = &[
    ("segment_native_entity_shapes", entity_shapes),
    ("segment_key_versions_and_mutation", keyed_versions),
    ("segment_bounded_scan_and_mutation", bounded_scan),
    ("segment_metadata_and_sealing", metadata_and_sealing),
];

fn row(id: u64, key: &str, value: i64) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
    entity.data = EntityData::Row(RowData::with_names(
        vec![Value::text(key), Value::Integer(value)],
        vec!["key".into(), "value".into()],
    ));
    entity
}

fn entity_shapes() {
    let entities = crate::record_corpus::entities();
    let mut segment = GrowingSegment::new(1, "mixed");
    for entity in &entities {
        segment
            .insert(entity.clone())
            .expect("canonical entity insert");
    }
    assert_eq!(segment.entity_count(), entities.len());
    for entity in &entities {
        let stored = segment.get(entity.id).expect("canonical entity retained");
        let encode = |entity: &UnifiedEntity| {
            let mut entity = entity.clone();
            // Storage assigns a fresh sequence on ordinary insert.
            entity.sequence_id = 0;
            let mut bytes = vec![];
            reddb_engine::storage::entity_codec::write_entity_binary(
                &mut bytes,
                &entity,
                reddb_file::STORE_VERSION_CURRENT,
            );
            bytes
        };
        assert_eq!(encode(stored), encode(entity));
    }
}

fn keyed_versions() {
    let mut segment = GrowingSegment::new(1, "keys");
    segment
        .bulk_insert(
            (1..=512)
                .map(|id| row(id, &format!("other-{id}"), 0))
                .collect(),
        )
        .expect("flat resident rows");
    let mut old = row(700, "wanted", 1);
    old.xmin = 2;
    let mut new = row(701, "wanted", 2);
    new.xmin = 3;
    segment.insert(old).expect("old keyed version");
    segment.insert(new).expect("new keyed version");
    assert_eq!(
        segment
            .find_key_entity("wanted", true, &mut |entity| entity.xmin < 3)
            .expect("snapshot selects old version")
            .id,
        EntityId::new(700)
    );
    assert_eq!(
        segment
            .find_key_entity("wanted", true, &mut |_| true)
            .expect("newest version")
            .id,
        EntityId::new(701)
    );
    *segment
        .get_mut(EntityId::new(701))
        .expect("mutable candidate") = row(701, "renamed", 3);
    assert_eq!(
        segment
            .find_key_entity("renamed", true, &mut |_| true)
            .expect("mutated key remains discoverable")
            .id,
        EntityId::new(701)
    );
    segment
        .delete(EntityId::new(700))
        .expect("delete old keyed version");
    assert!(segment
        .find_key_entity("wanted", true, &mut |_| true)
        .is_none());
}

fn bounded_scan() {
    let mut segment = GrowingSegment::new(1, "keys");
    segment
        .bulk_insert((1..=600).map(|id| row(id, "key", 0)).collect())
        .expect("flat rows");
    let mut cursor = segment.scan_cursor();
    // New appends cannot extend a cursor's captured boundary.
    segment.insert(row(900, "later", 1)).expect("late append");
    let mut ids = vec![];
    while !cursor.finished() {
        let before = ids.len();
        assert!(segment.scan_batch(&mut cursor, &mut || true, &mut |id| ids.push(id)));
        assert!(ids.len() - before <= SCAN_BATCH_SIZE);
    }
    assert_eq!(ids.len(), 600);
    assert!(!ids.contains(&EntityId::new(900)));
    let mut interrupted = segment.scan_cursor();
    assert!(
        !segment.scan_batch(&mut interrupted, &mut || false, &mut |_| panic!(
            "cancelled work"
        ))
    );
}

fn metadata_and_sealing() {
    let mut segment = GrowingSegment::new(1, "keys");
    segment.insert(row(1, "a", 1)).expect("row");
    segment
        .set_metadata(
            EntityId::new(1),
            Metadata::with_fields(HashMap::from([(
                "source".into(),
                MetadataValue::String("local".into()),
            )])),
        )
        .expect("metadata");
    assert!(
        matches!(segment.get_metadata(EntityId::new(1)).expect("metadata retained").get("source"),
        Some(MetadataValue::String(source)) if source == "local")
    );
    segment.seal().expect("seal resident storage");
    assert!(
        segment.insert(row(2, "b", 2)).is_err(),
        "sealed storage rejects writes"
    );
    assert_eq!(segment.iter().count(), 1);
}
