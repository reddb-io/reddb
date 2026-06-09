use super::*;
use crate::physical_metadata::BlockReference;

#[test]
fn native_store_dump_header_and_crc_footer_are_canonical() {
    let mut bytes = encode_native_store_header(STORE_VERSION_CURRENT);
    bytes.extend_from_slice(b"payload");
    append_native_store_crc32_footer(&mut bytes);

    let version = decode_native_store_header(&bytes).unwrap();
    assert_eq!(version, STORE_VERSION_CURRENT);

    let original_len = bytes.len();
    verify_native_store_crc32_footer(&mut bytes, version).unwrap();
    assert_eq!(bytes.len(), original_len - 4);
    assert_eq!(&bytes[0..4], STORE_MAGIC);
    assert_eq!(&bytes[8..], b"payload");

    let mut corrupt = encode_native_store_header(STORE_VERSION_CURRENT);
    corrupt.extend_from_slice(b"payload");
    append_native_store_crc32_footer(&mut corrupt);
    corrupt[8] ^= 0xff;
    assert!(verify_native_store_crc32_footer(&mut corrupt, STORE_VERSION_CURRENT).is_err());
}

#[test]
fn native_store_magic_matcher_is_canonical() {
    assert!(native_store_magic_matches(b"RDSTpayload"));
    assert!(!native_store_magic_matches(b"RDS"));
    assert!(!native_store_magic_matches(b"NOPEpayload"));
}

#[test]
fn native_entity_record_frame_round_trips_payloads() {
    let encoded = encode_native_entity_record_frame(b"entity", Some(b"metadata"));
    let decoded = decode_native_entity_record_frame(&encoded)
        .expect("decode frame")
        .expect("entity record frame");

    assert_eq!(decoded.entity, b"entity");
    assert_eq!(decoded.metadata, b"metadata");
}

#[test]
fn native_entity_record_frame_handles_empty_metadata_and_legacy_payloads() {
    let encoded = encode_native_entity_record_frame(b"entity", None);
    let decoded = decode_native_entity_record_frame(&encoded)
        .expect("decode frame")
        .expect("entity record frame");

    assert_eq!(decoded.entity, b"entity");
    assert_eq!(decoded.metadata, b"");
    assert!(decode_native_entity_record_frame(b"legacy-entity")
        .expect("decode legacy")
        .is_none());
}

#[test]
fn native_entity_record_frame_rejects_truncated_payloads() {
    let mut encoded = encode_native_entity_record_frame(b"entity", Some(b"metadata"));
    encoded.truncate(encoded.len() - 1);

    assert!(decode_native_entity_record_frame(&encoded).is_err());
}

#[test]
fn native_metadata_overflow_headers_round_trip() {
    let mut page1 = [0u8; METADATA_OVERFLOW_HEADER_BYTES];
    encode_native_metadata_overflow_header(
        &mut page1,
        NativeMetadataOverflowHeader {
            format_version: 9,
            total_payload_bytes: 1024,
            next_overflow_page_id: 42,
        },
    )
    .expect("encode page1 header");
    assert_eq!(
        decode_native_metadata_overflow_header(&page1)
            .expect("decode page1 header")
            .expect("overflow header"),
        NativeMetadataOverflowHeader {
            format_version: 9,
            total_payload_bytes: 1024,
            next_overflow_page_id: 42,
        }
    );
    assert!(decode_native_metadata_overflow_header(b"RDM2payload")
        .expect("decode non-overflow")
        .is_none());

    let mut continuation = [0u8; METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES];
    encode_native_metadata_overflow_continuation_header(
        &mut continuation,
        NativeMetadataOverflowContinuationHeader {
            next_overflow_page_id: 77,
            chunk_bytes: 2048,
        },
    )
    .expect("encode continuation header");
    assert_eq!(
        decode_native_metadata_overflow_continuation_header(&continuation)
            .expect("decode continuation header"),
        NativeMetadataOverflowContinuationHeader {
            next_overflow_page_id: 77,
            chunk_bytes: 2048,
        }
    );
}

#[test]
fn native_paged_metadata_header_round_trips_and_skips_legacy_payloads() {
    let mut bytes = Vec::new();
    encode_native_paged_metadata_header(
        &mut bytes,
        NativePagedMetadataHeader {
            format_version: 9,
            collection_count: 200,
        },
    );

    assert_eq!(
        decode_native_paged_metadata_header(&bytes)
            .expect("decode header")
            .expect("metadata header"),
        NativePagedMetadataHeader {
            format_version: 9,
            collection_count: 200,
        }
    );
    assert!(decode_native_paged_metadata_header(&123u32.to_le_bytes())
        .expect("decode legacy")
        .is_none());

    assert!(decode_native_paged_metadata_header(b"RDM2").is_err());
}

#[test]
fn native_len_prefixed_string_and_bytes_round_trip() {
    let mut bytes = Vec::new();
    encode_native_len_prefixed_str(&mut bytes, "collection");
    encode_native_len_prefixed_bytes(&mut bytes, b"\0payload");

    let mut pos = 0;
    assert_eq!(
        decode_native_len_prefixed_string(&bytes, &mut pos).expect("decode string"),
        "collection"
    );
    assert_eq!(
        decode_native_len_prefixed_bytes(&bytes, &mut pos).expect("decode bytes"),
        b"\0payload"
    );
    assert_eq!(pos, bytes.len());

    let mut truncated = bytes.clone();
    truncated.pop();
    let mut pos = 0;
    decode_native_len_prefixed_string(&truncated, &mut pos).expect("decode first string");
    assert!(decode_native_len_prefixed_bytes(&truncated, &mut pos).is_err());
}

#[test]
fn native_paged_collection_root_round_trips() {
    let mut bytes = Vec::new();
    encode_native_paged_collection_root(&mut bytes, "events", 42);

    let mut pos = 0;
    assert_eq!(
        decode_native_paged_collection_root(&bytes, &mut pos).expect("decode root"),
        NativePagedCollectionRoot {
            collection: "events".to_string(),
            root_page: 42,
        }
    );
    assert_eq!(pos, bytes.len());

    let mut truncated = bytes.clone();
    truncated.pop();
    let mut pos = 0;
    assert!(decode_native_paged_collection_root(&truncated, &mut pos).is_err());
}

#[test]
fn native_paged_cross_ref_round_trips() {
    let mut bytes = Vec::new();
    encode_native_paged_cross_ref(&mut bytes, 10, 20, 3, "accounts");

    let mut pos = 0;
    assert_eq!(
        decode_native_paged_cross_ref(&bytes, &mut pos).expect("decode cross-ref"),
        NativePagedCrossRef {
            source_id: 10,
            target_id: 20,
            ref_type: 3,
            target_collection: "accounts".to_string(),
        }
    );
    assert_eq!(pos, bytes.len());

    let mut truncated = bytes.clone();
    truncated.pop();
    let mut pos = 0;
    assert!(decode_native_paged_cross_ref(&truncated, &mut pos).is_err());
}

#[test]
fn native_dump_envelope_round_trips() {
    let mut bytes = Vec::new();
    encode_native_dump_count(&mut bytes, 1);
    encode_native_dump_collection_header(&mut bytes, "users", 2);
    encode_native_dump_entity_record(&mut bytes, b"entity-a");
    encode_native_dump_entity_record(&mut bytes, b"entity-b");
    encode_native_dump_count(&mut bytes, 1);
    encode_native_dump_cross_ref(&mut bytes, 10, 20, 4, "accounts");

    let mut pos = 0;
    assert_eq!(decode_native_dump_count(&bytes, &mut pos).unwrap(), 1);
    assert_eq!(
        decode_native_dump_collection_header(&bytes, &mut pos).unwrap(),
        NativeDumpCollectionHeader {
            name: "users".to_string(),
            entity_count: 2,
        }
    );
    assert_eq!(
        decode_native_dump_entity_record(&bytes, &mut pos).unwrap(),
        b"entity-a"
    );
    assert_eq!(
        decode_native_dump_entity_record(&bytes, &mut pos).unwrap(),
        b"entity-b"
    );
    assert_eq!(decode_native_dump_count(&bytes, &mut pos).unwrap(), 1);
    assert_eq!(
        decode_native_dump_cross_ref(&bytes, &mut pos).unwrap(),
        NativeDumpCrossRef {
            source_id: 10,
            target_id: 20,
            ref_type: 4,
            target_collection: "accounts".to_string(),
        }
    );
    assert_eq!(pos, bytes.len());

    let mut truncated = bytes.clone();
    truncated.pop();
    let mut pos = 0;
    assert!(decode_native_dump_count(&truncated, &mut pos).is_ok());
    assert!(decode_native_dump_collection_header(&truncated, &mut pos).is_ok());
    assert!(decode_native_dump_entity_record(&truncated, &mut pos).is_ok());
    assert!(decode_native_dump_entity_record(&truncated, &mut pos).is_ok());
    assert!(decode_native_dump_count(&truncated, &mut pos).is_ok());
    assert!(decode_native_dump_cross_ref(&truncated, &mut pos).is_err());
}

#[test]
fn native_collection_roots_page_round_trips() {
    let roots = BTreeMap::from([("events".to_string(), 10), ("users".to_string(), 42)]);
    let bytes = encode_native_collection_roots_page(&roots);
    assert_eq!(decode_native_collection_roots_page(&bytes).unwrap(), roots);
}

#[test]
fn native_manifest_summary_page_round_trips_sample() {
    let events: Vec<ManifestEvent> = (0..20)
        .map(|i| ManifestEvent {
            collection: "events".to_string(),
            object_key: format!("k{i}"),
            kind: ManifestEventKind::Checkpoint,
            block: BlockReference {
                index: i,
                checksum: i as u128 + 1,
            },
            snapshot_min: i,
            snapshot_max: Some(i + 100),
        })
        .collect();

    let bytes = encode_native_manifest_summary_page(7, &events);
    let decoded = decode_native_manifest_summary_page(&bytes).unwrap();
    assert_eq!(decoded.sequence, 7);
    assert_eq!(decoded.event_count, 20);
    assert!(!decoded.events_complete);
    assert_eq!(decoded.omitted_event_count, 4);
    assert_eq!(decoded.recent_events.len(), NATIVE_MANIFEST_SAMPLE_LIMIT);
    assert_eq!(decoded.recent_events[0].object_key, "k4");
}
