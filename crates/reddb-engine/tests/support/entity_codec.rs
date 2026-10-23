//! The same byte oracle and malformed-input cases run natively and in WASM.
use crate::codec_corpus as corpus;
use reddb_engine::storage::{entity::*, entity_codec::*};
use reddb_file::native_value::{read_native_value, write_native_value, NativeReader};
use reddb_types::Value;

pub const CASES: &[(&str, fn())] = &[
    ("entity_codec_native_v1_v12_goldens", native_goldens),
    ("entity_codec_value_tags_and_truncation", value_tags),
    ("entity_codec_invalid_lengths_and_depth", invalid_values),
    (
        "entity_codec_optional_tails_and_truncation",
        entity_boundaries,
    ),
];

fn native_goldens() {
    let inputs = corpus::entities();
    let mut position = 0;
    let bytes = include_bytes!("../fixtures/native-entity-v1-v12.bin");
    let mut reader = NativeReader::new(bytes, &mut position);
    let mut records = 0;
    while reader.remaining() != 0 {
        let version = u32::from_le_bytes(reader.fixed().expect("version"));
        let index = u32::from_le_bytes(reader.fixed().expect("index")) as usize;
        let length = u32::from_le_bytes(reader.fixed().expect("length")) as usize;
        let golden = reader.take(length).expect("golden bytes");
        let mut encoded = Vec::new();
        write_entity_binary(&mut encoded, &inputs[index], version);
        assert_eq!(
            encoded, golden,
            "old native writer V{version}, entity {index}"
        );
        let mut consumed = 0;
        let decoded = read_entity_binary(golden, &mut consumed, version).expect("golden decoded");
        assert_eq!(consumed, golden.len());
        assert_eq!(decoded.kind, inputs[index].kind);
        assert_eq!(decoded.id, inputs[index].id);
        assert_eq!(decoded.created_at, 123456);
        assert_eq!(decoded.updated_at, 123499);
        assert_eq!(decoded.sequence_id, 99);
        assert_eq!(decoded.embeddings()[0].vector, vec![0.5, 1.0]);
        assert_eq!(decoded.cross_refs()[0].target, EntityId::new(77));
        assert_eq!(decoded.has_explicit_logical_id(), version >= 8);
        assert_eq!(decoded.xmin, if version >= 9 { 17 } else { 0 });
        assert_eq!(decoded.xmax, if version >= 9 { 23 } else { 0 });
        match &decoded.data {
            EntityData::Row(row) if index == 0 => assert_eq!(row.columns, corpus::values()),
            EntityData::Vector(vector) => assert_eq!(
                vector.content.as_deref(),
                if version >= 6 {
                    Some("vector content")
                } else {
                    None
                }
            ),
            EntityData::TimeSeries(series) => {
                assert_eq!(
                    series.series_id,
                    if version >= 11 && index == 6 {
                        Some(42)
                    } else {
                        None
                    }
                );
                assert_eq!(series.tags.len(), usize::from(version >= 5 && index == 5));
                assert_eq!(series.fields.len(), usize::from(version >= 12));
            }
            EntityData::QueueMessage(message) => {
                assert_eq!(message.attempts, 2);
                assert_eq!(message.priority, if version >= 4 { Some(-3) } else { None });
                assert_eq!(message.max_attempts, if version >= 4 { 7 } else { 3 });
                assert_eq!(message.acked, version >= 4);
            }
            _ => {}
        }
        encoded.clear();
        write_entity_binary(&mut encoded, &decoded, version);
        assert_eq!(encoded, golden, "re-encoded V{version}, entity {index}");
        records += 1;
    }
    assert_eq!(records, 96);
}

fn value_tags() {
    for value in corpus::values() {
        let mut bytes = vec![];
        write_native_value(&mut bytes, &value);
        let mut position = 0;
        let decoded = read_native_value(&bytes, &mut position).expect("value");
        assert_eq!(decoded, value);
        assert_eq!(position, bytes.len());
        for end in 0..bytes.len() {
            assert!(
                read_native_value(&bytes[..end], &mut 0).is_err(),
                "truncated tag {} at {end}",
                bytes[0]
            );
        }
    }
    let mut bytes = vec![];
    write_native_value(&mut bytes, &Value::text("x".repeat(2047)));
    assert_eq!(bytes[0], 5);
    bytes.clear();
    write_native_value(&mut bytes, &Value::text("x".repeat(2048)));
    assert_eq!(bytes[0], 0x85);
    bytes.clear();
    write_native_value(&mut bytes, &Value::Blob(vec![7; 8192]));
    assert_eq!(bytes[0], 0x86);
}

fn invalid_values() {
    for bytes in [
        vec![255],
        vec![5, 255, 255, 255, 255, 15],
        vec![11, 255, 255, 255, 255, 15],
        vec![28, 255, 255, 255, 255, 15],
        vec![5, 1, 255],
        vec![5, 128, 128, 128, 128, 128, 0],
        vec![0x85, 1, 1, 0],
        vec![0x86, 255, 255, 255, 255, 15, 1, 0],
    ] {
        assert!(read_native_value(&bytes, &mut 0).is_err());
    }
    // Keep the declared length valid as a varint, but inconsistent with the
    // Zstd frame. Both compressed Text and Blob must reject it.
    for value in [Value::text("x".repeat(2048)), Value::Blob(vec![7; 8192])] {
        let mut bytes = vec![];
        write_native_value(&mut bytes, &value);
        bytes[1] ^= 1;
        assert!(read_native_value(&bytes, &mut 0).is_err());
    }
    let maximum = reddb_types::utils::json::JSON_MAX_NESTING_DEPTH as usize;
    let mut bytes = [28, 1].repeat(maximum);
    bytes.push(0);
    drop(read_native_value(&bytes, &mut 0).expect("supported nesting boundary"));
    bytes.splice(0..0, [28, 1]);
    assert!(read_native_value(&bytes, &mut 0).is_err());
    let mut position = usize::MAX;
    assert!(read_native_value(&[0], &mut position).is_err());
}

fn entity_boundaries() {
    for entity in corpus::entities() {
        let mut bytes = vec![];
        write_entity_binary(&mut bytes, &entity, 12);
        // Some old records intentionally omit optional tails. Any accepted
        // prefix must be re-encodable and retain the entity identity.
        for end in 0..bytes.len() {
            if let Ok(decoded) = read_entity_binary(&bytes[..end], &mut 0, 12) {
                assert_eq!(decoded.id, entity.id);
                let mut encoded = vec![];
                write_entity_binary(&mut encoded, &decoded, 12);
                assert!(read_entity_binary(&encoded, &mut 0, 12).is_ok());
            }
        }
    }
    let entity = UnifiedEntity::table_row(EntityId::new(42), "t", 1, vec![]);
    let mut old = vec![];
    write_entity_binary(&mut old, &entity, 7);
    let decoded = read_entity_binary(&old, &mut 0, 12).expect("legacy optional tail");
    assert_eq!(decoded.id, entity.id);
    assert_eq!(decoded.xmin, 0);
    assert!(!decoded.has_explicit_logical_id());
    assert!(read_entity_binary(&[1, 255], &mut 0, 12).is_err());
    assert!(read_entity_binary(&[1, 3, 0, 255], &mut 0, 12).is_err());
}
