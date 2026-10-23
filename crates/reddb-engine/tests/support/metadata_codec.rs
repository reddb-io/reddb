use super::{codec_corpus, metadata_corpus};
use reddb_engine::storage::{
    entity_codec::write_entity_binary,
    metadata::{Metadata, MetadataValue},
    metadata_codec::*,
};

pub const CASES: &[(&str, fn())] = &[
    ("metadata_codec_native_goldens", native_goldens),
    ("metadata_codec_record_envelopes", record_envelopes),
    ("metadata_codec_malformed_payloads", malformed_payloads),
    ("metadata_codec_depth_boundary", depth_boundary),
];

fn native_goldens() {
    let golden = include_bytes!("../fixtures/native-metadata.bin");
    let metadata = metadata_corpus::metadata();
    assert_eq!(serialize_metadata(Some(&metadata)), golden);
    assert_eq!(
        deserialize_metadata(golden)
            .expect("native metadata")
            .fields,
        metadata.fields
    );
    let decoded = deserialize_metadata(golden).expect("native metadata");
    assert_eq!(serialize_metadata(Some(&decoded)), golden);
    assert!(serialize_metadata(None).is_empty());
    assert!(serialize_metadata(Some(&Metadata::new())).is_empty());
    assert!(deserialize_metadata(&[0; 4])
        .expect("zero fields")
        .is_empty());
}

fn record_envelopes() {
    let metadata = metadata_corpus::metadata();
    for version in 1..=reddb_file::STORE_VERSION_CURRENT {
        for entity in codec_corpus::entities() {
            let mut bare = vec![];
            write_entity_binary(&mut bare, &entity, version);
            for metadata in [None, Some(&metadata)] {
                let record = serialize_entity_record(&entity, metadata, version);
                let expected = reddb_file::encode_native_entity_record_frame(
                    &bare,
                    metadata.map(|_| include_bytes!("../fixtures/native-metadata.bin").as_slice()),
                );
                assert_eq!(record, expected, "original native framing V{version}");
                let (decoded, decoded_metadata) =
                    deserialize_entity_record(&record, version).expect("framed record");
                assert_eq!(decoded.id, entity.id);
                assert_eq!(
                    decoded_metadata.as_ref().map(|m| &m.fields),
                    metadata.map(|m| &m.fields)
                );
            }
            let (decoded, metadata) =
                deserialize_entity_record(&bare, version).expect("legacy bare entity");
            assert_eq!(decoded.id, entity.id);
            assert!(metadata.is_none());
        }
    }
}

fn malformed_payloads() {
    let golden = include_bytes!("../fixtures/native-metadata.bin");
    for end in 0..golden.len() {
        assert!(
            deserialize_metadata(&golden[..end]).is_err(),
            "truncated at {end}"
        );
    }
    assert!(deserialize_metadata(&u32::MAX.to_le_bytes()).is_err());
    for tag in [6, 7, 11] {
        let mut bytes = vec![1, 0, 0, 0, 0, 0, 0, 0, tag];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            deserialize_metadata(&bytes).is_err(),
            "invalid count for tag {tag}"
        );
    }
    for tail in [vec![255], vec![10, 255], vec![4, 1, 0, 0, 0, 255]] {
        let mut bytes = vec![1, 0, 0, 0, 0, 0, 0, 0];
        bytes.extend(tail);
        assert!(deserialize_metadata(&bytes).is_err());
    }
    let entity = &codec_corpus::entities()[0];
    let mut bare = vec![];
    write_entity_binary(&mut bare, entity, 12);
    let record = reddb_file::encode_native_entity_record_frame(&bare, Some(&[255]));
    assert!(deserialize_entity_record(&record, 12).is_err());
}

fn depth_boundary() {
    let mut value = MetadataValue::Null;
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        value = MetadataValue::Array(vec![value]);
    }
    let mut metadata = Metadata::new();
    metadata.set("nested", value.clone());
    let bytes = serialize_metadata(Some(&metadata));
    assert_eq!(
        deserialize_metadata(&bytes)
            .expect("supported depth")
            .fields,
        metadata.fields
    );
    metadata.set("nested", MetadataValue::Array(vec![value]));
    assert!(deserialize_metadata(&serialize_metadata(Some(&metadata))).is_err());
}
