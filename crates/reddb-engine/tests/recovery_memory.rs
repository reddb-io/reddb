use reddb_engine::storage::entity::{EntityData, EntityId, RowData, UnifiedEntity};
use reddb_engine::storage::entity_codec::{read_entity_binary, write_entity_binary};
use reddb_engine::storage::manager::SegmentManager;
use reddb_file::native_store::STORE_VERSION_V11;
use reddb_types::Value;

fn named_row(id: u64, extra: bool) -> UnifiedEntity {
    let mut fields: Vec<String> = (0..16)
        .map(|i| format!("telemetry_dimension_{i:02}"))
        .collect();
    let mut values: Vec<Value> = (0..16).map(|i| Value::Integer(id as i64 + i)).collect();
    if extra {
        fields.push("later_added_dimension".into());
        values.push(Value::text("must survive"));
    }
    let mut row = UnifiedEntity::table_row(EntityId::new(id), "telemetry", id, vec![]);
    row.data = EntityData::Row(RowData::with_names(values, fields));
    row.created_at = 123456;
    row.updated_at = 123456;
    row
}

fn encoded(row: &UnifiedEntity) -> Vec<u8> {
    let mut out = Vec::new();
    write_entity_binary(&mut out, row, STORE_VERSION_V11);
    out
}

#[test]
fn named_snapshot_recovery_does_not_expand_the_resident_working_set() {
    let live = SegmentManager::new("telemetry");
    let recovered = SegmentManager::new("telemetry");
    live.bulk_insert((1..=4096).map(|id| named_row(id, false)).collect())
        .expect("valid recovery fixture");
    for id in 1..=4096 {
        let image = encoded(&named_row(id, false));
        let row =
            read_entity_binary(&image, &mut 0, STORE_VERSION_V11).expect("valid recovery fixture");
        recovered
            .restore_snapshot_entity(row)
            .expect("valid recovery fixture");
    }
    recovered.set_column_schema_if_empty(
        live.column_schema()
            .expect("valid recovery fixture")
            .as_ref()
            .clone(),
    );
    assert_eq!(recovered.compact_recovered_rows(), 0);
    eprintln!(
        "live={} recovered={}",
        live.resident_bytes(),
        recovered.resident_bytes()
    );
    assert!(
        recovered.resident_bytes() <= live.resident_bytes() * 5 / 4 + 131072,
        "reopening must not expand every row into a separate field-name hash table"
    );
    for id in [1, 2048, 4096] {
        assert_eq!(
            encoded(
                &recovered
                    .get(EntityId::new(id))
                    .expect("valid recovery fixture")
            ),
            encoded(&named_row(id, false)),
            "compaction must preserve named native records for another reopen"
        );
    }
    // Dynamic schemas must keep every field, including fields absent in the
    // first restored row. Compaction must never silently drop one.
    let changed = named_row(4097, true);
    recovered
        .restore_snapshot_entity(changed.clone())
        .expect("valid recovery fixture");
    assert_eq!(
        encoded(&recovered.get(changed.id).expect("valid recovery fixture")),
        encoded(&changed)
    );
}

#[test]
fn compaction_preserves_dynamic_shapes_and_captured_cursors() {
    let manager = SegmentManager::new("telemetry");
    for id in 1..=8 {
        let bytes = encoded(&named_row(id, false));
        manager
            .insert(
                read_entity_binary(&bytes, &mut 0, STORE_VERSION_V11)
                    .expect("valid recovery fixture"),
            )
            .expect("valid recovery fixture");
    }
    assert_eq!(
        manager.compact_recovered_rows(),
        0,
        "never guess a missing schema"
    );
    let original: Vec<_> = (1..=8)
        .map(|id| {
            encoded(
                &manager
                    .get(EntityId::new(id))
                    .expect("valid recovery fixture"),
            )
        })
        .collect();
    let schema = (0..16)
        .rev()
        .map(|i| format!("telemetry_dimension_{i:02}"))
        .collect();
    manager.set_column_schema_if_empty(schema);
    manager.force_seal().expect("valid recovery fixture");
    let cursor = manager.physical_scan_cursor();
    let before = manager.resident_bytes();
    assert_eq!(
        manager.compact_recovered_rows(),
        0,
        "captured segments stay immutable"
    );
    assert_eq!(manager.resident_bytes(), before);
    drop(cursor);
    assert_eq!(manager.compact_recovered_rows(), 8);
    for id in 1..=8 {
        let row = manager
            .get(EntityId::new(id))
            .expect("valid recovery fixture");
        assert_eq!(
            row.data.as_row().expect("valid recovery fixture").columns[0],
            Value::Integer(id as i64 + 15)
        );
        assert_eq!(
            encoded(&row),
            original[usize::try_from(id - 1).expect("small recovery fixture")]
        );
    }
    let extra = named_row(9, true);
    manager
        .restore_snapshot_entity(extra.clone())
        .expect("valid recovery fixture");
    assert_eq!(
        manager.compact_recovered_rows(),
        0,
        "dynamic fields are retained"
    );
    assert_eq!(
        encoded(&manager.get(extra.id).expect("valid recovery fixture")),
        encoded(&extra)
    );
}

#[test]
fn repeated_named_recovery_keeps_memory_and_every_native_record_stable() {
    let schema: Vec<_> = (0..16)
        .map(|i| format!("telemetry_dimension_{i:02}"))
        .collect();
    let mut images: Vec<_> = (1..=256).map(|id| encoded(&named_row(id, false))).collect();
    let original = images.clone();
    let mut warm_bytes = None;
    for _ in 0..32 {
        let recovered = SegmentManager::new("telemetry");
        for image in &images {
            recovered
                .restore_snapshot_entity(
                    read_entity_binary(image, &mut 0, STORE_VERSION_V11)
                        .expect("valid recovery fixture"),
                )
                .expect("valid recovery fixture");
        }
        recovered.set_column_schema_if_empty(schema.clone());
        assert_eq!(recovered.compact_recovered_rows(), 0);
        let bytes = recovered.resident_bytes();
        assert_eq!(*warm_bytes.get_or_insert(bytes), bytes);
        images = (1..=256)
            .map(|id| {
                encoded(
                    &recovered
                        .get(EntityId::new(id))
                        .expect("valid recovery fixture"),
                )
            })
            .collect();
        assert_eq!(
            images, original,
            "recovery cannot change durable row identity"
        );
    }
}

#[test]
fn recovery_compacts_each_row_when_its_schema_is_already_known() {
    let manager = SegmentManager::new("telemetry");
    manager.set_column_schema_if_empty(
        (0..16)
            .map(|i| format!("telemetry_dimension_{i:02}"))
            .collect(),
    );
    for id in 1..=256 {
        let bytes = encoded(&named_row(id, false));
        manager
            .restore_snapshot_entity(
                read_entity_binary(&bytes, &mut 0, STORE_VERSION_V11)
                    .expect("valid recovery fixture"),
            )
            .expect("valid recovery fixture");
        let row = manager
            .get(EntityId::new(id))
            .expect("valid recovery fixture");
        assert!(
            row.data
                .as_row()
                .expect("valid recovery fixture")
                .named
                .is_none(),
            "compaction after the full snapshot loads is too late for its memory peak"
        );
        assert_eq!(encoded(&row), bytes);
    }
}

#[test]
fn recovery_shares_dynamic_row_shapes_without_declaring_columns_or_unbounded_cache() {
    let manager = SegmentManager::new("dynamic");
    for id in 1..=64 {
        let mut row = UnifiedEntity::table_row(EntityId::new(id), "dynamic", id, vec![]);
        row.data = EntityData::Row(RowData::with_names(
            vec![Value::Integer(id as i64)],
            vec![format!("field_{id}")],
        ));
        let bytes = encoded(&row);
        manager
            .restore_snapshot_entity(
                read_entity_binary(&bytes, &mut 0, STORE_VERSION_V11)
                    .expect("valid recovery fixture"),
            )
            .expect("valid recovery fixture");
        let recovered = manager.get(row.id).expect("valid recovery fixture");
        assert_eq!(encoded(&recovered), bytes);
        assert_eq!(
            recovered
                .data
                .as_row()
                .expect("valid recovery fixture")
                .named
                .is_none(),
            id <= 16,
            "shape cache is capped"
        );
    }
    assert!(
        manager.column_schema().is_none(),
        "row shapes are not a declared collection schema"
    );
}

#[test]
fn compiled_projection_rejects_a_different_explicit_row_shape() {
    let mut entity = UnifiedEntity::table_row(EntityId::new(1), "dynamic", 1, vec![]);
    entity.data = EntityData::Row(RowData {
        columns: vec![Value::Integer(999), Value::Integer(7)],
        named: None,
        schema: Some(std::sync::Arc::new(vec!["a_dimension".into(), "id".into()])),
    });
    let columns = vec!["id".into()];
    assert!(
        reddb_engine::query::records::runtime_table_record_with_col_indices(
            &entity,
            &columns,
            &[(0, 0)]
        )
        .is_none()
    );
    let row = reddb_engine::query::records::runtime_table_record_from_entity_ref_projected(
        &entity, &columns,
    )
    .expect("valid recovery fixture");
    assert_eq!(row.get("id"), Some(&Value::Integer(7)));
}

#[test]
fn recovery_preserves_named_system_metadata_for_bootstrap_readers() {
    for collection in ["red_config", "red_materialized_view_defs", "red.config"] {
        let manager = SegmentManager::new(collection);
        let mut entity = UnifiedEntity::table_row(EntityId::new(1), collection, 1, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![
                Value::text("system.bootstrap.completed"),
                Value::Boolean(true),
            ],
            vec!["key".into(), "value".into()],
        ));
        let image = encoded(&entity);
        manager
            .restore_snapshot_entity(entity)
            .expect("restore bootstrap marker");
        manager.set_column_schema_if_empty(vec!["key".into(), "value".into()]);
        manager.compact_recovered_rows();
        let recovered = manager.get(EntityId::new(1)).expect("marker survives");
        let fields = recovered
            .data
            .as_row()
            .expect("metadata row")
            .named
            .as_ref()
            .expect("system readers retain their named representation");
        assert_eq!(fields.get("value"), Some(&Value::Boolean(true)));
        assert_eq!(encoded(&recovered), image);
    }
}

#[test]
fn recovered_documents_keep_promoted_fields_in_lean_reads() {
    use reddb_engine::query::records::{runtime_table_record_lean, runtime_table_record_lean_ref};
    let manager = SegmentManager::new("documents");
    let mut body = Vec::new();
    reddb_types::document_body_codec::encode(
        &[
            ("name", &Value::text("alice")),
            ("score", &Value::Integer(42)),
        ],
        &mut body,
    )
    .expect("body");
    let mut original = UnifiedEntity::table_row(EntityId::new(1), "documents", 1, vec![]);
    original.data = EntityData::Row(RowData::with_names(
        vec![Value::Json(body)],
        vec!["body".into()],
    ));
    manager
        .restore_snapshot_entity(original.clone())
        .expect("recover document");
    let recovered = manager.get(original.id).expect("recovered document");
    assert!(recovered.data.as_row().expect("row").named.is_none());
    for entity in [original, recovered] {
        for record in [
            runtime_table_record_lean(entity.clone()),
            runtime_table_record_lean_ref(&entity),
        ] {
            let record = record.expect("document record");
            assert_eq!(record.get("name"), Some(&Value::text("alice")));
            assert_eq!(record.get("score"), Some(&Value::Integer(42)));
        }
    }
}
