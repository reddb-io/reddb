use super::*;

#[test]
fn physical_metadata_documents_validate_json_before_publish() {
    assert!(encode_physical_metadata_json_document(r#"{"ok":true}"#).is_ok());
    assert!(encode_physical_metadata_binary_document(r#"{"ok":true}"#).is_ok());
    assert!(encode_physical_metadata_json_document("{").is_err());
    assert!(encode_physical_metadata_binary_document("{").is_err());
}

#[test]
fn physical_metadata_document_decode_rejects_invalid_json() {
    assert_eq!(
        decode_physical_metadata_document(br#"{"sequence":1}"#).unwrap(),
        r#"{"sequence":1}"#
    );
    assert!(decode_physical_metadata_document(b"not-json").is_err());
}

#[test]
fn physical_metadata_journal_paths_are_listed_and_pruned_by_file_contract() {
    let root = std::env::temp_dir().join(format!(
        "reddb-file-physical-journal-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let data_path = root.join("main.rdb");

    for sequence in [1, 2, 3] {
        fs::write(
            crate::layout::physical_metadata_journal_path(&data_path, sequence),
            b"{}",
        )
        .unwrap();
    }
    fs::write(root.join("main.rdb.meta.rdbx.not-a-journal"), b"{}").unwrap();

    let paths = list_physical_metadata_journal_paths(&data_path).unwrap();
    assert_eq!(paths.len(), 3);
    assert!(paths[0].ends_with("main.rdb.meta.rdbx.seq-00000000000000000001"));

    prune_physical_metadata_journal_paths(&data_path, 1).unwrap();
    let paths = list_physical_metadata_journal_paths(&data_path).unwrap();
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("main.rdb.meta.rdbx.seq-00000000000000000003"));
    assert!(root.join("main.rdb.meta.rdbx.not-a-journal").exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn physical_metadata_document_root_envelope_round_trips() {
    let mut ttl = BTreeMap::new();
    ttl.insert("events".to_string(), 86_400_000);
    let document = PhysicalMetadataDocumentEnvelope {
        protocol_version: PHYSICAL_METADATA_PROTOCOL_VERSION.to_string(),
        generated_at_unix_ms: 123,
        last_loaded_from: Some("binary".to_string()),
        last_healed_at_unix_ms: Some(456),
        manifest_json: r#"{"format_version":2}"#.to_string(),
        catalog_json: r#"{"total_collections":1}"#.to_string(),
        manifest_events_json: vec![r#"{"kind":"checkpoint"}"#.to_string()],
        indexes_json: vec![r#"{"name":"idx"}"#.to_string()],
        graph_projections_json: vec![r#"{"name":"graph"}"#.to_string()],
        analytics_jobs_json: vec![r#"{"id":"job"}"#.to_string()],
        tree_definitions_json: vec![r#"{"name":"tree"}"#.to_string()],
        collection_ttl_defaults_ms: ttl,
        collection_contracts_json: vec![r#"{"name":"events"}"#.to_string()],
        hypertables_json: vec![r#"{"name":"metrics"}"#.to_string()],
        exports_json: vec![r#"{"name":"dump"}"#.to_string()],
        superblock_json: r#"{"sequence":"9"}"#.to_string(),
        snapshots_json: vec![r#"{"snapshot_id":"9"}"#.to_string()],
    };

    let json = encode_physical_metadata_document_root_json(&document, false).unwrap();
    assert!(json.contains("\"protocol_version\""));
    assert!(json.contains("\"manifest_events\""));
    assert!(json.contains("\"collection_ttl_defaults_ms\""));

    let decoded = decode_physical_metadata_document_root_json(&json).unwrap();
    assert_eq!(decoded.protocol_version, document.protocol_version);
    assert_eq!(decoded.generated_at_unix_ms, 123);
    assert_eq!(decoded.last_loaded_from.as_deref(), Some("binary"));
    assert_eq!(decoded.last_healed_at_unix_ms, Some(456));
    assert_eq!(decoded.manifest_events_json.len(), 1);
    assert_eq!(
        decoded.collection_ttl_defaults_ms.get("events"),
        Some(&86_400_000)
    );
    assert_eq!(decoded.snapshots_json.len(), 1);
}

#[test]
fn physical_schema_manifest_round_trips() {
    let mut metadata = BTreeMap::new();
    metadata.insert("owner".to_string(), "ops".to_string());
    let manifest = PhysicalSchemaManifest {
        format_version: 9,
        created_at_unix_ms: 123,
        updated_at_unix_ms: 456,
        collection_count: 7,
        options: PhysicalSchemaOptions {
            mode: "persistent".to_string(),
            data_path: Some("/var/lib/reddb/main.rdb".to_string()),
            read_only: false,
            create_if_missing: true,
            verify_checksums: true,
            durability_mode: Some("wal_durable_grouped".to_string()),
            group_commit_window_ms: Some(8),
            group_commit_max_statements: Some(9),
            group_commit_max_wal_bytes: Some(10),
            auto_checkpoint_pages: 11,
            cache_pages: 12,
            snapshot_retention: Some(13),
            export_retention: Some(14),
            force_create: false,
            capabilities: vec!["table".to_string(), "graph".to_string()],
            metadata,
        },
    };

    let json = encode_physical_schema_manifest_json(&manifest).unwrap();
    assert!(json.contains("\"capabilities\""));
    assert!(json.contains("\"group_commit_window_ms\""));

    let decoded = decode_physical_schema_manifest_json(&json).unwrap();
    assert_eq!(decoded, manifest);
}

#[test]
fn physical_catalog_snapshot_round_trips() {
    let mut stats = BTreeMap::new();
    stats.insert(
        "events".to_string(),
        PhysicalCatalogCollectionStats {
            entities: 10,
            cross_refs: 2,
            segments: 1,
        },
    );
    let catalog = PhysicalCatalogSnapshot {
        name: "main".to_string(),
        total_entities: 10,
        total_collections: 1,
        updated_at_unix_ms: 123,
        stats_by_collection: stats,
    };

    let json = encode_physical_catalog_snapshot_json(&catalog).unwrap();
    assert!(json.contains("\"stats_by_collection\""));
    assert_eq!(
        decode_physical_catalog_snapshot_json(&json).unwrap(),
        catalog
    );
}

#[test]
fn physical_analytical_storage_round_trips() {
    let config = PhysicalAnalyticalStorageConfig {
        columnar: true,
        time_key: "ts".to_string(),
        order_by_key: Some("tenant_id".to_string()),
    };

    let json = encode_physical_analytical_storage_json(&config).unwrap();
    assert!(json.contains("\"columnar\""));
    assert_eq!(
        decode_physical_analytical_storage_json(&json).unwrap(),
        config
    );
}

#[test]
fn physical_subscription_descriptor_round_trips() {
    let subscription = PhysicalSubscriptionDescriptor {
        name: "audit".to_string(),
        source: "events".to_string(),
        target_queue: "audit_queue".to_string(),
        ops_filter: vec!["insert".to_string(), "delete".to_string()],
        where_filter: Some("tenant_id = current_tenant()".to_string()),
        redact_fields: vec!["secret".to_string()],
        enabled: true,
        all_tenants: false,
    };

    let json = encode_physical_subscription_descriptor_json(&subscription).unwrap();
    assert!(json.contains("\"target_queue\""));
    assert_eq!(
        decode_physical_subscription_descriptor_json(&json).unwrap(),
        subscription
    );
}

#[test]
fn physical_analytics_view_descriptor_round_trips() {
    let view = PhysicalAnalyticsViewDescriptor {
        output: "communities".to_string(),
        algorithm: Some("louvain".to_string()),
        resolution: Some(1.25),
        max_iterations: Some(42),
        tolerance: Some(0.0001),
    };

    let json = encode_physical_analytics_view_descriptor_json(&view).unwrap();
    assert!(json.contains("\"max_iterations\""));
    assert_eq!(
        decode_physical_analytics_view_descriptor_json(&json).unwrap(),
        view
    );
}

#[test]
fn physical_declared_column_contract_round_trips() {
    let column = PhysicalDeclaredColumnContract {
        name: "amount".to_string(),
        data_type: "DECIMAL(12, 2)".to_string(),
        sql_type: Some(PhysicalSqlTypeName {
            name: "DECIMAL".to_string(),
            modifiers: vec![
                PhysicalTypeModifier::Number(12),
                PhysicalTypeModifier::Type(Box::new(PhysicalSqlTypeName {
                    name: "VARCHAR".to_string(),
                    modifiers: vec![PhysicalTypeModifier::Number(32)],
                })),
            ],
        }),
        not_null: true,
        default: Some("0".to_string()),
        compress: Some(3),
        unique: false,
        primary_key: false,
        enum_variants: vec!["pending".to_string(), "paid".to_string()],
        array_element: Some("TEXT".to_string()),
        decimal_precision: Some(12),
    };

    let json = encode_physical_declared_column_contract_json(&column).unwrap();
    assert!(json.contains("\"enum_variants\""));
    assert_eq!(
        decode_physical_declared_column_contract_json(&json).unwrap(),
        column
    );
}

#[test]
fn physical_collection_contract_round_trips() {
    let contract = PhysicalCollectionContract {
        name: "orders".to_string(),
        declared_model: "table".to_string(),
        schema_mode: "strict".to_string(),
        origin: "explicit".to_string(),
        version: 7,
        created_at_unix_ms: 100,
        updated_at_unix_ms: 200,
        default_ttl_ms: Some(30_000),
        vector_dimension: Some(128),
        vector_metric: Some("cosine".to_string()),
        context_index_fields: vec!["body".to_string()],
        declared_columns: vec![PhysicalDeclaredColumnContract {
            name: "id".to_string(),
            data_type: "UUID".to_string(),
            sql_type: Some(PhysicalSqlTypeName {
                name: "UUID".to_string(),
                modifiers: Vec::new(),
            }),
            not_null: true,
            default: None,
            compress: None,
            unique: true,
            primary_key: true,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        }],
        table_def_hex: Some("00ff".to_string()),
        timestamps_enabled: true,
        context_index_enabled: false,
        metrics_raw_retention_ms: Some(60_000),
        metrics_rollup_policies: vec!["1m".to_string()],
        metrics_tenant_identity: Some("tenant_id".to_string()),
        metrics_namespace: Some("default".to_string()),
        append_only: true,
        subscriptions: vec![PhysicalSubscriptionDescriptor {
            name: "audit".to_string(),
            source: "orders".to_string(),
            target_queue: "audit_queue".to_string(),
            ops_filter: vec!["insert".to_string()],
            where_filter: None,
            redact_fields: Vec::new(),
            enabled: true,
            all_tenants: false,
        }],
        analytics_config: vec![PhysicalAnalyticsViewDescriptor {
            output: "centrality".to_string(),
            algorithm: Some("pagerank".to_string()),
            resolution: None,
            max_iterations: Some(20),
            tolerance: Some(0.01),
        }],
        session_key: Some("session_id".to_string()),
        session_gap_ms: Some(10_000),
        retention_duration_ms: Some(86_400_000),
        analytical_storage: Some(PhysicalAnalyticalStorageConfig {
            columnar: true,
            time_key: "ts".to_string(),
            order_by_key: Some("id".to_string()),
        }),
        ai_policy: None,
    };

    let json = encode_physical_collection_contract_json(&contract).unwrap();
    assert!(json.contains("\"context_index_enabled\""));
    assert_eq!(
        decode_physical_collection_contract_json(&json).unwrap(),
        contract
    );
}

#[test]
fn physical_collection_contract_with_ai_policy_round_trips() {
    let contract = PhysicalCollectionContract {
        name: "posts".to_string(),
        declared_model: "table".to_string(),
        schema_mode: "strict".to_string(),
        origin: "explicit".to_string(),
        version: 1,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 2,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def_hex: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,
        ai_policy: Some(PhysicalAiPolicy {
            embed: Some(PhysicalAiEmbedPolicy {
                fields: vec!["title".to_string(), "body".to_string()],
                provider: "openai".to_string(),
                model: "text-embedding-3-small".to_string(),
            }),
            moderate: Some(PhysicalAiModeratePolicy {
                fields: vec!["body".to_string()],
                provider: "openai".to_string(),
                model: "omni-moderation-latest".to_string(),
                sync_gate: true,
                degraded_mode: "closed".to_string(),
                reject_action: "flag".to_string(),
                hard_delete_on_reject: true,
            }),
            vision: Some(PhysicalAiVisionPolicy {
                image_field: "photo".to_string(),
                output_kinds: vec!["caption".to_string(), "tags".to_string()],
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
            }),
        }),
    };

    let json = encode_physical_collection_contract_json(&contract).expect("encode ai policy");
    assert!(json.contains("\"hard_delete_on_reject\""));
    assert_eq!(
        decode_physical_collection_contract_json(&json).expect("decode ai policy"),
        contract
    );

    // A present-but-empty policy round-trips through the Null modality arms.
    let bare = PhysicalCollectionContract {
        ai_policy: Some(PhysicalAiPolicy {
            embed: None,
            moderate: None,
            vision: None,
        }),
        ..contract
    };
    let json = encode_physical_collection_contract_json(&bare).expect("encode bare policy");
    assert_eq!(
        decode_physical_collection_contract_json(&json).expect("decode bare policy"),
        bare
    );
}

#[test]
fn physical_metadata_core_contracts_round_trip() {
    let mut roots = BTreeMap::new();
    roots.insert("docs".to_string(), 42);
    let superblock = SuperblockHeader {
        format_version: 2,
        sequence: u64::MAX,
        copies: 4,
        manifest: ManifestPointers {
            oldest: BlockReference {
                index: 1,
                checksum: u128::MAX,
            },
            newest: BlockReference {
                index: 2,
                checksum: 99,
            },
        },
        free_set: BlockReference {
            index: 3,
            checksum: 100,
        },
        collection_roots: roots,
    };
    let json = encode_physical_superblock_json(&superblock).unwrap();
    assert!(
        json.contains(&format!("\"sequence\":\"{}\"", u64::MAX)),
        "large u64 values stay string-encoded for legacy compatibility: {json}"
    );
    assert!(
        json.contains(&format!("\"checksum\":\"{}\"", u128::MAX)),
        "large u128 values stay string-encoded for legacy compatibility: {json}"
    );
    assert_eq!(
        decode_physical_superblock_json(&json)
            .unwrap()
            .manifest
            .oldest
            .checksum,
        u128::MAX
    );

    let event = ManifestEvent {
        collection: "docs".to_string(),
        object_key: "a".to_string(),
        kind: ManifestEventKind::Checkpoint,
        block: BlockReference {
            index: 7,
            checksum: 8,
        },
        snapshot_min: 9,
        snapshot_max: Some(10),
    };
    let event_json = encode_physical_manifest_event_json(&event).unwrap();
    let decoded = decode_physical_manifest_event_json(&event_json).unwrap();
    assert_eq!(decoded.collection, "docs");
    assert_eq!(decoded.kind, ManifestEventKind::Checkpoint);
    assert_eq!(decoded.snapshot_max, Some(10));

    let reference = physical_manifest_block_reference(7, 9);
    assert_eq!(reference.index, 7);
    assert_eq!(reference.checksum, ((7u128) << 64) | 9u128);
    assert_eq!(physical_superblock_object_key(9), "superblock:9");
    let checkpoint = physical_superblock_checkpoint_event(9);
    assert_eq!(checkpoint.collection, PHYSICAL_SYSTEM_COLLECTION);
    assert_eq!(checkpoint.object_key, "superblock:9");
    assert_eq!(checkpoint.kind, ManifestEventKind::Checkpoint);
    assert_eq!(checkpoint.block, physical_manifest_block_reference(9, 9));

    let snapshot = SnapshotDescriptor {
        snapshot_id: 11,
        created_at_unix_ms: 12,
        superblock_sequence: 13,
        collection_count: 14,
        total_entities: 15,
    };
    let snapshot_json = encode_physical_snapshot_descriptor_json(&snapshot).unwrap();
    assert_eq!(
        decode_physical_snapshot_descriptor_json(&snapshot_json)
            .unwrap()
            .snapshot_id,
        11
    );

    let export = ExportDescriptor {
        name: "daily".to_string(),
        created_at_unix_ms: 16,
        snapshot_id: Some(17),
        superblock_sequence: 18,
        data_path: "data.rdb".to_string(),
        metadata_path: "data.meta.rdbx".to_string(),
        collection_count: 19,
        total_entities: 20,
    };
    let export_json = encode_physical_export_descriptor_json(&export).unwrap();
    let decoded_export = decode_physical_export_descriptor_json(&export_json).unwrap();
    assert_eq!(decoded_export.name, "daily");
    assert_eq!(decoded_export.snapshot_id, Some(17));

    let projection = PhysicalGraphProjection {
        name: "g".to_string(),
        created_at_unix_ms: 21,
        updated_at_unix_ms: 22,
        state: "ready".to_string(),
        source: "docs".to_string(),
        node_labels: vec!["Person".to_string()],
        node_types: vec!["person".to_string()],
        edge_labels: vec!["KNOWS".to_string()],
        last_materialized_sequence: Some(23),
    };
    let projection_json = encode_physical_graph_projection_json(&projection).unwrap();
    let decoded_projection = decode_physical_graph_projection_json(&projection_json).unwrap();
    assert_eq!(decoded_projection.node_labels, vec!["Person"]);
    assert_eq!(decoded_projection.last_materialized_sequence, Some(23));

    let mut metadata = BTreeMap::new();
    metadata.insert("k".to_string(), "v".to_string());
    let job = PhysicalAnalyticsJob {
        id: "job".to_string(),
        kind: "materialize".to_string(),
        state: "queued".to_string(),
        projection: Some("g".to_string()),
        created_at_unix_ms: 24,
        updated_at_unix_ms: 25,
        last_run_sequence: Some(26),
        metadata,
    };
    let job_json = encode_physical_analytics_job_json(&job).unwrap();
    let decoded_job = decode_physical_analytics_job_json(&job_json).unwrap();
    assert_eq!(decoded_job.projection.as_deref(), Some("g"));
    assert_eq!(decoded_job.metadata.get("k").map(String::as_str), Some("v"));

    let tree = PhysicalTreeDefinition {
        collection: "docs".to_string(),
        name: "comments".to_string(),
        root_id: 27,
        default_max_children: 28,
        ordered_children: true,
        ownership: "owned".to_string(),
        auto_fix_mode: "conservative".to_string(),
        created_at_unix_ms: 29,
        updated_at_unix_ms: 30,
    };
    let tree_json = encode_physical_tree_definition_json(&tree).unwrap();
    let decoded_tree = decode_physical_tree_definition_json(&tree_json).unwrap();
    assert_eq!(decoded_tree.root_id, 27);
    assert!(decoded_tree.ordered_children);

    let index = PersistedPhysicalIndexState {
        name: "idx_docs".to_string(),
        kind: "btree".to_string(),
        collection: Some("docs".to_string()),
        enabled: true,
        entries: 31,
        estimated_memory_bytes: 32,
        last_refresh_ms: Some(33),
        backend: "native".to_string(),
        artifact_kind: Some("btree".to_string()),
        artifact_root_page: Some(34),
        artifact_checksum: Some(35),
        build_state: "ready".to_string(),
    };
    let index_json = encode_persisted_physical_index_state_json(&index).unwrap();
    let decoded_index = decode_persisted_physical_index_state_json(&index_json).unwrap();
    assert_eq!(decoded_index.kind, "btree");
    assert_eq!(decoded_index.artifact_checksum, Some(35));

    let hypertable = PersistedPhysicalHypertable {
        name: "metrics".to_string(),
        time_column: "ts".to_string(),
        chunk_interval_ns: 36,
        default_ttl_ns: Some(37),
        chunks: vec![PersistedPhysicalHypertableChunk {
            start_ns: 38,
            end_ns_exclusive: 39,
            row_count: 40,
            min_ts_ns: 41,
            max_ts_ns: 42,
            sealed: true,
            ttl_override_ns: Some(43),
            columnar_page: Some(PhysicalPageLocation {
                page_id: 44,
                offset: 45,
                length: 46,
            }),
            columnar_derived: true,
        }],
    };
    let hypertable_json = encode_persisted_physical_hypertable_json(&hypertable).unwrap();
    let decoded_hypertable = decode_persisted_physical_hypertable_json(&hypertable_json).unwrap();
    assert_eq!(decoded_hypertable.name, "metrics");
    assert_eq!(
        decoded_hypertable.chunks[0].columnar_page,
        Some(PhysicalPageLocation {
            page_id: 44,
            offset: 45,
            length: 46,
        })
    );
    assert!(decoded_hypertable.chunks[0].columnar_derived);
}
