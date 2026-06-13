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

#[test]
fn native_store_header_and_crc_reject_bad_inputs() {
    assert!(decode_native_store_header(b"short").is_err());

    let mut bad_magic = encode_native_store_header(STORE_VERSION_CURRENT);
    bad_magic[0] = b'X';
    assert!(decode_native_store_header(&bad_magic).is_err());

    let unsupported = encode_native_store_header(STORE_VERSION_CURRENT + 1);
    assert!(decode_native_store_header(&unsupported).is_err());
    assert!(is_supported_store_version(STORE_VERSION_V1));
    assert!(is_supported_store_version(STORE_VERSION_V9));
    assert!(!is_supported_store_version(STORE_VERSION_CURRENT + 1));

    let mut legacy = encode_native_store_header(STORE_VERSION_V2);
    legacy.extend_from_slice(b"payload");
    verify_native_store_crc32_footer(&mut legacy, STORE_VERSION_V2).unwrap();
    assert_eq!(&legacy[8..], b"payload");

    let mut too_short = encode_native_store_header(STORE_VERSION_CURRENT);
    assert!(verify_native_store_crc32_footer(&mut too_short, STORE_VERSION_CURRENT).is_err());
}

#[test]
fn native_metadata_overflow_headers_reject_short_buffers() {
    let mut short = [0u8; METADATA_OVERFLOW_HEADER_BYTES - 1];
    assert!(encode_native_metadata_overflow_header(
        &mut short,
        NativeMetadataOverflowHeader {
            format_version: 1,
            total_payload_bytes: 2,
            next_overflow_page_id: 3,
        },
    )
    .is_err());
    assert!(decode_native_metadata_overflow_header(b"RDM3short").is_err());
    assert!(decode_native_metadata_overflow_header(b"NOPE")
        .unwrap()
        .is_none());

    let mut continuation = [0u8; METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES - 1];
    assert!(encode_native_metadata_overflow_continuation_header(
        &mut continuation,
        NativeMetadataOverflowContinuationHeader {
            next_overflow_page_id: 1,
            chunk_bytes: 2,
        },
    )
    .is_err());
    assert!(decode_native_metadata_overflow_continuation_header(&continuation).is_err());
}

#[test]
fn native_low_level_decoders_reject_invalid_utf8_and_varints() {
    let mut invalid_utf8 = Vec::new();
    encode_native_len_prefixed_bytes(&mut invalid_utf8, &[0xff]);
    let mut pos = 0;
    assert!(decode_native_len_prefixed_string(&invalid_utf8, &mut pos).is_err());

    let mut invalid_collection = vec![0x01];
    invalid_collection.extend_from_slice(&[0xff]);
    invalid_collection.push(0);
    let mut pos = 0;
    assert!(decode_native_dump_collection_header(&invalid_collection, &mut pos).is_err());

    let invalid_varu32 = [0x80, 0x80, 0x80, 0x80, 0x80];
    let mut pos = 0;
    assert!(decode_native_dump_count(&invalid_varu32, &mut pos).is_err());

    let mut invalid_varu64 = vec![0x80; 10];
    invalid_varu64.push(0);
    let mut pos = 0;
    assert!(decode_native_dump_cross_ref(&invalid_varu64, &mut pos).is_err());
}

#[test]
fn native_collection_roots_decode_tolerates_partial_tail_and_rejects_bad_utf8() {
    assert!(decode_native_collection_roots_page(b"NOPE").is_err());

    let mut roots = BTreeMap::new();
    roots.insert("users".to_string(), 1);
    roots.insert("events".to_string(), 2);
    let mut bytes = encode_native_collection_roots_page(&roots);
    bytes.truncate(bytes.len() - 3);
    let decoded = decode_native_collection_roots_page(&bytes).unwrap();
    assert_eq!(decoded.len(), 1);

    let mut invalid = Vec::new();
    invalid.extend_from_slice(NATIVE_COLLECTION_ROOTS_MAGIC);
    invalid.extend_from_slice(&1u32.to_le_bytes());
    invalid.extend_from_slice(&1u32.to_le_bytes());
    invalid.push(0xff);
    invalid.extend_from_slice(&1u64.to_le_bytes());
    assert!(decode_native_collection_roots_page(&invalid).is_err());
}

#[test]
fn native_manifest_summary_covers_all_kinds_none_and_unknown_snapshot_flags() {
    let events = vec![
        ManifestEvent {
            collection: "c".to_string(),
            object_key: "i".to_string(),
            kind: ManifestEventKind::Insert,
            block: BlockReference {
                index: 1,
                checksum: 11,
            },
            snapshot_min: 1,
            snapshot_max: None,
        },
        ManifestEvent {
            collection: "c".to_string(),
            object_key: "u".to_string(),
            kind: ManifestEventKind::Update,
            block: BlockReference {
                index: 2,
                checksum: 22,
            },
            snapshot_min: 2,
            snapshot_max: Some(12),
        },
        ManifestEvent {
            collection: "c".to_string(),
            object_key: "r".to_string(),
            kind: ManifestEventKind::Remove,
            block: BlockReference {
                index: 3,
                checksum: 33,
            },
            snapshot_min: 3,
            snapshot_max: None,
        },
        ManifestEvent {
            collection: "c".to_string(),
            object_key: "ck".to_string(),
            kind: ManifestEventKind::Checkpoint,
            block: BlockReference {
                index: 4,
                checksum: 44,
            },
            snapshot_min: 4,
            snapshot_max: Some(14),
        },
    ];

    let bytes = encode_native_manifest_summary_page(9, &events);
    let decoded = decode_native_manifest_summary_page(&bytes).unwrap();
    assert_eq!(decoded.sequence, 9);
    assert!(decoded.events_complete);
    assert_eq!(
        decoded
            .recent_events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["insert", "update", "remove", "checkpoint"]
    );
    assert_eq!(decoded.recent_events[0].snapshot_max, None);
    assert_eq!(decoded.recent_events[1].snapshot_max, Some(12));

    assert!(decode_native_manifest_summary_page(b"NOPE").is_err());

    let mut unknown_kind = bytes.clone();
    unknown_kind[25] = 99;
    assert_eq!(
        decode_native_manifest_summary_page(&unknown_kind)
            .unwrap()
            .recent_events[0]
            .kind,
        "unknown"
    );

    let mut unknown_snapshot_flag = bytes.clone();
    let flag_pos = 25 + 1 + 2 + 1 + 2 + 1 + 8 + 16 + 8;
    unknown_snapshot_flag[flag_pos] = 7;
    assert_eq!(
        decode_native_manifest_summary_page(&unknown_snapshot_flag)
            .unwrap()
            .recent_events[0]
            .snapshot_max,
        None
    );

    let mut truncated_snapshot_max = bytes.clone();
    let second_flag = flag_pos + 1 + (1 + 2 + 1 + 2 + 1 + 8 + 16 + 8);
    truncated_snapshot_max[second_flag] = 1;
    truncated_snapshot_max.truncate(second_flag + 1);
    assert!(decode_native_manifest_summary_page(&truncated_snapshot_max).is_err());
}

#[test]
fn native_registry_summary_round_trips_full_samples() {
    let mut metadata = BTreeMap::new();
    metadata.insert("window".to_string(), "daily".to_string());

    let summary = NativeRegistrySummary {
        collection_count: 2,
        index_count: 2,
        graph_projection_count: 1,
        analytics_job_count: 1,
        vector_artifact_count: 1,
        collections_complete: true,
        indexes_complete: false,
        graph_projections_complete: true,
        analytics_jobs_complete: false,
        vector_artifacts_complete: true,
        omitted_collection_count: 0,
        omitted_index_count: 1,
        omitted_graph_projection_count: 0,
        omitted_analytics_job_count: 2,
        omitted_vector_artifact_count: 0,
        collection_names: vec!["users".to_string(), "events".to_string()],
        indexes: vec![
            NativeRegistryIndexSummary {
                name: "idx_users_email".to_string(),
                kind: "btree".to_string(),
                collection: Some("users".to_string()),
                enabled: true,
                entries: 10,
                estimated_memory_bytes: 2048,
                last_refresh_ms: Some(123),
                backend: "native".to_string(),
            },
            NativeRegistryIndexSummary {
                name: "idx_global".to_string(),
                kind: "vector".to_string(),
                collection: None,
                enabled: false,
                entries: 0,
                estimated_memory_bytes: 0,
                last_refresh_ms: None,
                backend: "cold".to_string(),
            },
        ],
        graph_projections: vec![NativeRegistryProjectionSummary {
            name: "social".to_string(),
            source: "users".to_string(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            node_labels: vec!["User".to_string()],
            node_types: vec!["person".to_string()],
            edge_labels: vec!["FOLLOWS".to_string()],
            last_materialized_sequence: Some(77),
        }],
        analytics_jobs: vec![NativeRegistryJobSummary {
            id: "job-1".to_string(),
            kind: "rollup".to_string(),
            projection: Some("social".to_string()),
            state: "ready".to_string(),
            created_at_unix_ms: 3,
            updated_at_unix_ms: 4,
            last_run_sequence: Some(88),
            metadata,
        }],
        vector_artifacts: vec![NativeVectorArtifactSummary {
            collection: "vectors".to_string(),
            artifact_kind: "hnsw".to_string(),
            vector_count: 100,
            dimension: 384,
            max_layer: 4,
            serialized_bytes: 4096,
            checksum: 55,
        }],
    };

    let bytes = encode_native_registry_summary_page(&summary);
    assert_eq!(
        decode_native_registry_summary_page(&bytes).unwrap(),
        summary
    );
    assert!(decode_native_registry_summary_page(b"NOPE").is_err());

    let mut truncated_tail = bytes.clone();
    truncated_tail.truncate(truncated_tail.len() - 1);
    assert!(decode_native_registry_summary_page(&truncated_tail).is_ok());
}

#[test]
fn native_recovery_catalog_metadata_blob_and_vector_pages_round_trip() {
    let recovery = NativeRecoverySummary {
        snapshot_count: 2,
        export_count: 2,
        snapshots_complete: true,
        exports_complete: false,
        omitted_snapshot_count: 0,
        omitted_export_count: 1,
        snapshots: vec![NativeSnapshotSummary {
            snapshot_id: 1,
            created_at_unix_ms: 2,
            superblock_sequence: 3,
            collection_count: 4,
            total_entities: 5,
        }],
        exports: vec![
            NativeExportSummary {
                name: "exp-a".to_string(),
                created_at_unix_ms: 6,
                snapshot_id: Some(7),
                superblock_sequence: 8,
                collection_count: 9,
                total_entities: 10,
            },
            NativeExportSummary {
                name: "exp-b".to_string(),
                created_at_unix_ms: 11,
                snapshot_id: None,
                superblock_sequence: 12,
                collection_count: 13,
                total_entities: 14,
            },
        ],
    };
    let bytes = encode_native_recovery_summary_page(&recovery);
    assert_eq!(
        decode_native_recovery_summary_page(&bytes).unwrap(),
        recovery
    );
    assert!(decode_native_recovery_summary_page(b"NOPE").is_err());

    let catalog = NativeCatalogSummary {
        collection_count: 1,
        total_entities: 10,
        collections_complete: true,
        omitted_collection_count: 0,
        collections: vec![NativeCatalogCollectionSummary {
            name: "users".to_string(),
            entities: 10,
            cross_refs: 2,
            segments: 3,
        }],
    };
    let bytes = encode_native_catalog_summary_page(&catalog);
    assert_eq!(decode_native_catalog_summary_page(&bytes).unwrap(), catalog);
    assert!(decode_native_catalog_summary_page(b"NOPE").is_err());

    let metadata = NativeMetadataStateSummary {
        protocol_version: "v1".to_string(),
        generated_at_unix_ms: 1,
        last_loaded_from: Some("disk".to_string()),
        last_healed_at_unix_ms: Some(2),
    };
    let bytes = encode_native_metadata_state_summary_page(&metadata);
    assert_eq!(
        decode_native_metadata_state_summary_page(&bytes).unwrap(),
        metadata
    );
    assert!(decode_native_metadata_state_summary_page(b"NOPE").is_err());

    let no_optional_metadata = NativeMetadataStateSummary {
        protocol_version: "v1".to_string(),
        generated_at_unix_ms: 1,
        last_loaded_from: None,
        last_healed_at_unix_ms: None,
    };
    let bytes = encode_native_metadata_state_summary_page(&no_optional_metadata);
    assert_eq!(
        decode_native_metadata_state_summary_page(&bytes).unwrap(),
        no_optional_metadata
    );

    assert_eq!(native_blob_chunk_capacity(128, 16), 100);
    let blob = encode_native_blob_page(42, b"chunk");
    assert_eq!(
        decode_native_blob_page(&blob).unwrap(),
        (42, b"chunk".to_vec())
    );
    assert!(decode_native_blob_page(b"NOPE").is_err());
    let mut truncated_blob = blob.clone();
    truncated_blob.pop();
    assert!(decode_native_blob_page(&truncated_blob).is_err());

    let artifacts = vec![NativeVectorArtifactPageSummary {
        collection: "vectors".to_string(),
        artifact_kind: "ivf".to_string(),
        root_page: 4,
        page_count: 5,
        byte_len: 6,
        checksum: 7,
    }];
    let bytes = encode_native_vector_artifact_store_page(&artifacts);
    assert_eq!(
        decode_native_vector_artifact_store_page(&bytes).unwrap(),
        artifacts
    );
    assert!(decode_native_vector_artifact_store_page(b"NOPE").is_err());
}

#[test]
fn native_summary_decoders_tolerate_partial_samples_without_panics() {
    let registry = NativeRegistrySummary {
        collection_count: 1,
        index_count: 1,
        graph_projection_count: 1,
        analytics_job_count: 1,
        vector_artifact_count: 1,
        collections_complete: false,
        indexes_complete: false,
        graph_projections_complete: false,
        analytics_jobs_complete: false,
        vector_artifacts_complete: false,
        omitted_collection_count: 0,
        omitted_index_count: 0,
        omitted_graph_projection_count: 0,
        omitted_analytics_job_count: 0,
        omitted_vector_artifact_count: 0,
        collection_names: vec!["users".to_string()],
        indexes: vec![NativeRegistryIndexSummary {
            name: "idx".to_string(),
            kind: "btree".to_string(),
            collection: Some("users".to_string()),
            enabled: true,
            entries: 1,
            estimated_memory_bytes: 2,
            last_refresh_ms: None,
            backend: "native".to_string(),
        }],
        graph_projections: vec![NativeRegistryProjectionSummary {
            name: "p".to_string(),
            source: "users".to_string(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            node_labels: vec![],
            node_types: vec![],
            edge_labels: vec![],
            last_materialized_sequence: None,
        }],
        analytics_jobs: vec![NativeRegistryJobSummary {
            id: "j".to_string(),
            kind: "rollup".to_string(),
            projection: None,
            state: "new".to_string(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            last_run_sequence: None,
            metadata: BTreeMap::new(),
        }],
        vector_artifacts: vec![NativeVectorArtifactSummary {
            collection: "vectors".to_string(),
            artifact_kind: "hnsw".to_string(),
            vector_count: 1,
            dimension: 2,
            max_layer: 3,
            serialized_bytes: 4,
            checksum: 5,
        }],
    };

    let mut bytes = encode_native_registry_summary_page(&registry);
    bytes.truncate(90);
    assert!(decode_native_registry_summary_page(&bytes).is_err());

    let mut recovery = encode_native_recovery_summary_page(&NativeRecoverySummary {
        snapshot_count: 1,
        export_count: 1,
        snapshots_complete: false,
        exports_complete: false,
        omitted_snapshot_count: 0,
        omitted_export_count: 0,
        snapshots: vec![NativeSnapshotSummary {
            snapshot_id: 1,
            created_at_unix_ms: 2,
            superblock_sequence: 3,
            collection_count: 4,
            total_entities: 5,
        }],
        exports: vec![NativeExportSummary {
            name: "exp".to_string(),
            created_at_unix_ms: 6,
            snapshot_id: Some(7),
            superblock_sequence: 8,
            collection_count: 9,
            total_entities: 10,
        }],
    });
    recovery.truncate(40);
    assert!(decode_native_recovery_summary_page(&recovery)
        .unwrap()
        .snapshots
        .is_empty());

    let mut catalog = encode_native_catalog_summary_page(&NativeCatalogSummary {
        collection_count: 1,
        total_entities: 1,
        collections_complete: false,
        omitted_collection_count: 0,
        collections: vec![NativeCatalogCollectionSummary {
            name: "users".to_string(),
            entities: 1,
            cross_refs: 2,
            segments: 3,
        }],
    });
    catalog.truncate(catalog.len() - 2);
    assert!(decode_native_catalog_summary_page(&catalog)
        .unwrap()
        .collections
        .is_empty());

    let mut metadata = encode_native_metadata_state_summary_page(&NativeMetadataStateSummary {
        protocol_version: "v1".to_string(),
        generated_at_unix_ms: 1,
        last_loaded_from: None,
        last_healed_at_unix_ms: Some(2),
    });
    metadata.truncate(metadata.len() - 1);
    assert!(decode_native_metadata_state_summary_page(&metadata).is_err());

    let mut vector_page =
        encode_native_vector_artifact_store_page(&[NativeVectorArtifactPageSummary {
            collection: "vectors".to_string(),
            artifact_kind: "hnsw".to_string(),
            root_page: 1,
            page_count: 2,
            byte_len: 3,
            checksum: 4,
        }]);
    vector_page.truncate(vector_page.len() - 1);
    assert!(decode_native_vector_artifact_store_page(&vector_page)
        .unwrap()
        .is_empty());
}
