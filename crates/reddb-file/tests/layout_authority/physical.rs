use crate::common::*;

#[test]
fn server_does_not_redeclare_physical_metadata_core_json_format() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/physical/json_codec.rs"));
    let server_non_test = server
        .split("#[cfg(test)]")
        .next()
        .expect("physical/json_codec.rs has non-test source");
    let file = read(root.join("crates/reddb-file/src/physical_metadata.rs"));

    for forbidden in [
        "\"collection_roots\"",
        "\"free_set\"",
        "\"object_key\"",
        "\"snapshot_min\"",
        "\"snapshot_max\"",
        "\"oldest\"",
        "\"newest\"",
        "\"checksum\"",
        "\"node_labels\"",
        "\"edge_labels\"",
        "\"last_materialized_sequence\"",
        "\"last_run_sequence\"",
        "\"default_max_children\"",
        "\"auto_fix_mode\"",
        "\"estimated_memory_bytes\"",
        "\"artifact_root_page\"",
        "\"artifact_checksum\"",
        "\"start_ns\"",
        "\"end_ns_exclusive\"",
        "\"columnar_page\"",
        "\"page_id\"",
        "unsupported manifest event kind",
        "superblock.roots",
    ] {
        assert!(
            !server_non_test.contains(forbidden),
            "physical metadata JSON field contract belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "encode_physical_superblock_json",
        "decode_physical_superblock_json",
        "encode_physical_manifest_event_json",
        "decode_physical_manifest_event_json",
        "encode_physical_snapshot_descriptor_json",
        "decode_physical_snapshot_descriptor_json",
        "encode_physical_export_descriptor_json",
        "decode_physical_export_descriptor_json",
        "encode_physical_graph_projection_json",
        "decode_physical_graph_projection_json",
        "encode_physical_analytics_job_json",
        "decode_physical_analytics_job_json",
        "encode_physical_tree_definition_json",
        "decode_physical_tree_definition_json",
        "encode_persisted_physical_index_state_json",
        "decode_persisted_physical_index_state_json",
        "encode_persisted_physical_hypertable_json",
        "decode_persisted_physical_hypertable_json",
        "\"collection_roots\"",
        "\"snapshot_min\"",
        "\"checksum\"",
        "\"node_labels\"",
        "\"last_run_sequence\"",
        "\"default_max_children\"",
        "\"estimated_memory_bytes\"",
        "\"columnar_page\"",
        "\"page_id\"",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own physical metadata JSON contract {required}"
        );
    }

    for required in [
        "reddb_file::encode_physical_superblock_json",
        "reddb_file::decode_physical_superblock_json",
        "reddb_file::encode_physical_manifest_event_json",
        "reddb_file::decode_physical_manifest_event_json",
        "reddb_file::encode_physical_snapshot_descriptor_json",
        "reddb_file::decode_physical_snapshot_descriptor_json",
        "reddb_file::encode_physical_export_descriptor_json",
        "reddb_file::decode_physical_export_descriptor_json",
        "reddb_file::encode_physical_graph_projection_json",
        "reddb_file::decode_physical_graph_projection_json",
        "reddb_file::encode_physical_analytics_job_json",
        "reddb_file::decode_physical_analytics_job_json",
        "reddb_file::encode_physical_tree_definition_json",
        "reddb_file::decode_physical_tree_definition_json",
        "reddb_file::encode_persisted_physical_index_state_json",
        "reddb_file::decode_persisted_physical_index_state_json",
        "reddb_file::encode_persisted_physical_hypertable_json",
        "reddb_file::decode_persisted_physical_hypertable_json",
        "file_json_to_server_json",
    ] {
        assert!(
            server.contains(required),
            "server physical JSON codec should delegate through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_physical_metadata_paths() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/physical/metadata_file.rs"));

    for forbidden in [
        ".meta.json",
        ".meta.rdbx",
        ".seq-{sequence:020}",
        ".export.",
        "sanitize_export_name",
    ] {
        assert!(
            !text.contains(forbidden),
            "physical metadata filename contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::layout::physical_metadata_json_path",
        "reddb_file::layout::physical_metadata_binary_path",
        "reddb_file::layout::physical_metadata_journal_path",
        "reddb_file::layout::physical_export_data_path",
        "reddb_file::list_physical_metadata_journal_paths",
        "reddb_file::prune_physical_metadata_journal_paths",
        "reddb_file::read_physical_metadata_document",
        "reddb_file::write_physical_metadata_json_document",
        "reddb_file::write_physical_metadata_binary_document",
    ] {
        assert!(
            text.contains(required),
            "physical metadata pathing should route through {required}"
        );
    }
    for forbidden in [
        "fs::read_dir",
        "starts_with(&prefix)",
        "fs::remove_file(path)",
    ] {
        assert!(
            !text.contains(forbidden),
            "physical metadata journal discovery/pruning belongs in reddb-file, found {forbidden:?}"
        );
    }
}

#[test]
fn server_does_not_own_physical_metadata_document_codec() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/physical/metadata_file.rs"));
    let json_codec = read(root.join("crates/reddb-server/src/physical/json_codec.rs"));
    let json_codec_non_test = json_codec
        .split("#[cfg(test)]")
        .next()
        .expect("json_codec.rs has non-test source");
    let file = read(root.join("crates/reddb-file/src/physical_metadata.rs"));

    for forbidden in [
        "fs::read_to_string(path)",
        "fs::read(path)",
        "fs::write(path, text)",
        "fs::write(path, bytes)",
        "from_slice::<JsonValue>",
        "to_vec(&self.to_json_value())",
        "root.insert(",
        "\"protocol_version\".to_string()",
        "\"generated_at_unix_ms\".to_string()",
        "\"last_loaded_from\".to_string()",
        "\"last_healed_at_unix_ms\".to_string()",
        "\"manifest_events\".to_string()",
        "\"collection_ttl_defaults_ms\".to_string()",
        "json_required(object, \"manifest\")",
        "json_required(object, \"catalog\")",
        "json_required(object, \"superblock\")",
        "json_required(object, \"snapshots\")",
    ] {
        assert!(
            !text.contains(forbidden),
            "physical metadata document codec belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "PhysicalMetadataDocumentEnvelope",
        "encode_physical_metadata_document_root_json",
        "decode_physical_metadata_document_root_json",
        "PhysicalSchemaManifest",
        "PhysicalSchemaOptions",
        "PhysicalCatalogSnapshot",
        "PhysicalCatalogCollectionStats",
        "PhysicalAnalyticalStorageConfig",
        "PhysicalSubscriptionDescriptor",
        "PhysicalAnalyticsViewDescriptor",
        "PhysicalDeclaredColumnContract",
        "PhysicalCollectionContract",
        "PhysicalSqlTypeName",
        "PhysicalTypeModifier",
        "encode_physical_schema_manifest_json",
        "decode_physical_schema_manifest_json",
        "encode_physical_catalog_snapshot_json",
        "decode_physical_catalog_snapshot_json",
        "encode_physical_analytical_storage_json",
        "decode_physical_analytical_storage_json",
        "encode_physical_subscription_descriptor_json",
        "decode_physical_subscription_descriptor_json",
        "encode_physical_analytics_view_descriptor_json",
        "decode_physical_analytics_view_descriptor_json",
        "encode_physical_declared_column_contract_json",
        "decode_physical_declared_column_contract_json",
        "encode_physical_collection_contract_json",
        "decode_physical_collection_contract_json",
        "\"protocol_version\".to_string()",
        "\"manifest_events\".to_string()",
        "\"collection_ttl_defaults_ms\".to_string()",
        "\"stats_by_collection\".to_string()",
        "\"total_entities\".to_string()",
        "\"total_collections\".to_string()",
        "\"columnar\".to_string()",
        "\"time_key\".to_string()",
        "\"order_by_key\".to_string()",
        "\"target_queue\".to_string()",
        "\"ops_filter\".to_string()",
        "\"redact_fields\".to_string()",
        "\"all_tenants\".to_string()",
        "\"max_iterations\".to_string()",
        "\"tolerance\".to_string()",
        "\"data_type\".to_string()",
        "\"sql_type\".to_string()",
        "\"enum_variants\".to_string()",
        "\"decimal_precision\".to_string()",
        "\"declared_model\".to_string()",
        "\"schema_mode\".to_string()",
        "\"context_index_enabled\".to_string()",
        "\"metrics_raw_retention_ms\".to_string()",
        "\"analytics_config\".to_string()",
        "\"table_def\".to_string()",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own physical metadata document root {required}"
        );
    }

    for forbidden in [
        "\"durability_mode\".to_string()",
        "\"group_commit_window_ms\".to_string()",
        "\"group_commit_max_statements\".to_string()",
        "\"group_commit_max_wal_bytes\".to_string()",
        "\"snapshot_retention\".to_string()",
        "\"export_retention\".to_string()",
        "\"capabilities\".to_string()",
        "expect_object(value, \"manifest\")",
        "json_required(object, \"options\")",
        "\"stats_by_collection\".to_string()",
        "\"total_entities\".to_string()",
        "\"total_collections\".to_string()",
        "json_required(object, \"stats_by_collection\")",
        "json_usize_required(entry, \"entities\")",
        "json_usize_required(object, \"total_entities\")",
        "\"columnar\".to_string()",
        "\"time_key\".to_string()",
        "\"order_by_key\".to_string()",
        "expect_object(value, \"analytical_storage\")",
        "\"target_queue\".to_string()",
        "\"ops_filter\".to_string()",
        "\"redact_fields\".to_string()",
        "\"all_tenants\".to_string()",
        "expect_object(value, \"subscription_descriptor\")",
        "\"max_iterations\".to_string()",
        "\"tolerance\".to_string()",
        "expect_object(value, \"analytics_view_descriptor\")",
        "\"data_type\".to_string()",
        "\"sql_type\".to_string()",
        "\"enum_variants\".to_string()",
        "\"decimal_precision\".to_string()",
        "expect_object(value, \"declared_column_contract\")",
        "expect_object(value, \"type_modifier\")",
        "unsupported type modifier kind",
        "\"declared_model\".to_string()",
        "\"schema_mode\".to_string()",
        "\"context_index_enabled\".to_string()",
        "\"metrics_raw_retention_ms\".to_string()",
        "\"analytics_config\".to_string()",
        "\"table_def\".to_string()",
        "expect_object(value, \"collection_contract\")",
    ] {
        assert!(
            !json_codec_non_test.contains(forbidden),
            "physical metadata fragment codec belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "encode_physical_schema_manifest_json",
        "decode_physical_schema_manifest_json",
        "encode_physical_catalog_snapshot_json",
        "decode_physical_catalog_snapshot_json",
        "encode_physical_analytical_storage_json",
        "decode_physical_analytical_storage_json",
        "encode_physical_subscription_descriptor_json",
        "decode_physical_subscription_descriptor_json",
        "schema_manifest_to_persisted",
        "schema_manifest_from_persisted",
        "catalog_to_persisted",
        "catalog_from_persisted",
        "analytical_storage_to_persisted",
        "analytical_storage_from_persisted",
        "subscription_descriptor_to_persisted",
        "subscription_descriptor_from_persisted",
        "encode_physical_analytics_view_descriptor_json",
        "decode_physical_analytics_view_descriptor_json",
        "analytics_view_descriptor_to_persisted",
        "analytics_view_descriptor_from_persisted",
        "encode_physical_declared_column_contract_json",
        "decode_physical_declared_column_contract_json",
        "declared_column_contract_to_persisted",
        "declared_column_contract_from_persisted",
        "encode_physical_collection_contract_json",
        "decode_physical_collection_contract_json",
        "collection_contract_to_persisted",
        "collection_contract_from_persisted",
    ] {
        assert!(
            json_codec_non_test.contains(required),
            "server physical metadata adapter should route through {required}"
        );
    }
}

#[test]
fn server_does_not_redeclare_shm_file_format() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/physical/shm.rs"));

    for forbidden in [
        "pub const SHM_MAGIC",
        "pub const SHM_VERSION",
        "pub const SHM_HEADER_SIZE",
        "pub const SHM_FILE_SIZE",
        "pub struct ShmHeader",
        "fn fold_checksum",
        "[0u8; SHM_HEADER_SIZE]",
        ".encode()",
        "ShmHeader::decode",
    ] {
        assert!(
            !text.contains(forbidden),
            "SHM binary file format belongs in reddb-file, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("pub use reddb_file::{ShmHeader"),
        "server should reexport the SHM file contract from reddb-file"
    );
    for required in [
        "reddb_file::initialize_shm_file",
        "reddb_file::read_shm_header_from_file",
        "reddb_file::write_shm_header_to_file",
    ] {
        assert!(
            text.contains(required),
            "server SHM runtime should route file-format operations through {required}"
        );
    }
}

#[test]
fn server_does_not_own_spill_file_format() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/storage/cache/spill.rs"));
    let server_non_test = server
        .split("#[cfg(test)]")
        .next()
        .expect("spill.rs has non-test source");
    let file = read(root.join("crates/reddb-file/src/spill.rs"));

    for forbidden in [
        "reddb-spill",
        "e == \"spill\"",
        "b\"SPIL\"",
        "write_all(&[2u8])",
        "checksum.to_le_bytes()",
        "data.len() as u64).to_le_bytes()",
        "u32::from_le_bytes(checksum_bytes)",
        "u64::from_le_bytes(size_bytes)",
        "version[0]",
        "wrapping_add(b as u32)",
        "crc32::crc32",
    ] {
        assert!(
            !server_non_test.contains(forbidden),
            "spill file frame belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "DEFAULT_SPILL_DIR_NAME",
        "SPILL_FILE_EXTENSION",
        "default_spill_dir",
        "is_spill_file_path",
        "SPILL_FILE_MAGIC",
        "SPILL_FILE_HEADER_LEN",
        "encode_spill_file_frame",
        "decode_spill_file_frame",
        "spill_file_name",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own spill contract {required}"
        );
    }

    assert!(server_non_test.contains("reddb_file::default_spill_dir"));
    assert!(server_non_test.contains("reddb_file::is_spill_file_path"));
    assert!(server_non_test.contains("reddb_file::encode_spill_file_frame"));
    assert!(server_non_test.contains("reddb_file::decode_spill_file_frame"));
    assert!(server_non_test.contains("reddb_file::spill_file_name"));
}
