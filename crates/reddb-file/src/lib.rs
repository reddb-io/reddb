//! RedDB file artifact layer.
//!
//! This crate owns RedDB's file-level contracts: embedded single-file `.rdb`,
//! serverless boot artifacts, and primary-replica file/WAL planning. Runtime,
//! SQL, and storage-engine payload semantics stay in `reddb-server`; this crate
//! works with bytes, offsets, locks, checkpoints, manifests, and recovery rules.

pub mod ai_model_cache;
pub mod backup_manifest;
pub mod backup_temp;
pub mod blob_cache;
pub mod bloom_segment;
pub mod btree_value_layout;
pub mod column_block;
pub mod control_store;
pub mod embedded;
pub mod file_format;
pub mod graph_label_registry;
pub mod graph_record;
pub mod graph_store;
pub mod graph_table_index;
pub mod layout;
pub mod local_backend;
pub mod logical_wal;
pub mod native_store;
pub mod operational_manifest;
pub mod physical_metadata;
pub mod physical_metadata_policy;
pub mod primary_replica;
pub mod profile;
pub mod serverless;
pub mod shm;
pub mod spill;
pub mod store_wal;
pub mod transaction_wal;
pub mod turboquant_snapshot;
pub mod vector_btree_page_format;
pub mod vector_value_codec;
pub mod wal_header;
pub mod wal_record;
pub mod zone_map;

pub use ai_model_cache::{
    ai_model_cache_manifest_path, ai_model_cache_manifest_temp_path, ai_model_cache_purge_dir,
    ai_model_cache_purge_root, ai_model_cache_root, ai_model_cache_staging_dir,
    ai_model_cache_staging_root, copy_ai_model_cache_artifact, decode_ai_model_cache_manifest_json,
    encode_ai_model_cache_manifest_json, AiModelCacheManifest, AiModelCacheManifestFile,
    AI_MODEL_CACHE_DIR_NAME, AI_MODEL_CACHE_MANIFEST_FILE, AI_MODEL_CACHE_PURGE_DIR_NAME,
    AI_MODEL_CACHE_STAGING_DIR_NAME,
};
pub use backup_manifest::{
    archived_snapshot_key, archived_wal_segment_key, backup_head_artifact, backup_head_key,
    backup_root_from_snapshot_prefix, backup_snapshot_dir, backup_snapshot_dir_prefix,
    backup_snapshot_prefix, backup_wal_dir, backup_wal_dir_prefix, backup_wal_prefix,
    decode_archived_logical_wal_records, decode_backup_head_json, decode_snapshot_manifest_json,
    decode_unified_manifest_json, decode_wal_segment_manifest_json,
    encode_archived_logical_wal_records, encode_backup_head_json, encode_snapshot_manifest_json,
    encode_unified_manifest_json, encode_wal_segment_manifest_json, is_archived_snapshot_key,
    is_archived_wal_segment_key, is_backup_manifest_sidecar_key, parse_archived_snapshot_key,
    parse_archived_wal_segment_key, remote_database_key, sha256_bytes_hex, sha256_file_hex,
    snapshot_manifest_artifact, snapshot_manifest_key, unified_manifest_artifact,
    unified_manifest_key, wal_segment_manifest_artifact, wal_segment_manifest_key,
    ArchivedLogicalWalRecord, BackupHead, BackupJsonArtifact, SnapshotManifest, UnifiedManifest,
    UnifiedSnapshotEntry, UnifiedWalEntry, WalSegmentManifest, WalSegmentMeta,
    BACKUP_MANIFEST_FORMAT_VERSION,
};
pub use backup_temp::{
    BackupTempJsonFile, ARCHIVED_CHANGE_RECORDS_READ_TEMP_PREFIX,
    ARCHIVED_CHANGE_RECORDS_TEMP_PREFIX, BACKUP_JSON_OBJECT_READ_TEMP_PREFIX,
    BACKUP_JSON_OBJECT_TEMP_PREFIX,
};
pub use blob_cache::{
    blob_cache_control_path, blob_cache_control_temp_path, blob_cache_double_write_path,
    blob_cache_l2_backup_control_key, blob_cache_l2_backup_pager_key, decode_l2_v2_frame,
    encode_l2_key, encode_l2_v2_frame, L2BlobFrame, L2Control, L2Record, L2_BACKUP_CONTROL_SUFFIX,
    L2_BACKUP_PAGER_SUFFIX, L2_BLOB_MAGIC, L2_CONTROL_MAGIC, L2_FORMAT_V1_RAW, L2_FORMAT_V2_FRAMED,
    L2_FRAME_TAG_RAW, L2_FRAME_TAG_ZSTD, L2_METADATA_MAGIC,
};
pub use bloom_segment::{
    decode_bloom_segment_frame, encode_bloom_segment_frame, BloomSegmentFrame,
    BloomSegmentFrameError, BLOOM_SEGMENT_HEADER_LEN,
};
pub use btree_value_layout::{
    btree_projected_cell_len, btree_value_pointer_head, decode_btree_inline_payload,
    decode_btree_value_cell, encode_btree_inline_compressed, encode_btree_inline_raw,
    encode_btree_pointer, BTreeValueCell, BTreeValueCellError, BTREE_VALUE_MAX_SIZE,
    BTREE_VALUE_OVERFLOW_THRESHOLD, BTREE_VALUE_POINTER_CELL_LEN,
};
pub use column_block::{
    column_block_crc32, decode_column_block_frame, decode_column_block_granule_bloom_blob,
    decode_column_block_granule_index_blob, encode_column_block_frame,
    encode_column_block_granule_bloom_blob, encode_column_block_granule_index_blob,
    peek_column_block_version, ColumnBlockColumn, ColumnBlockFrame, ColumnBlockFrameError,
    ColumnBlockGranuleBloom, ColumnBlockGranuleIndex, ColumnBlockGranuleStats, ColumnBlockPart,
    COLUMN_BLOCK_DIR_ENTRY_LEN, COLUMN_BLOCK_FOOTER_LEN, COLUMN_BLOCK_HEADER_LEN,
};
pub use control_store::{
    DurableLastVote, FileLastVoteStore, FileTermStore, DEFAULT_FILE_TERM, LAST_VOTE_TEMP_EXTENSION,
    TERM_TEMP_EXTENSION,
};
pub use embedded::{
    EmbeddedRdbArtifact, EmbeddedRdbManifest, EmbeddedRdbOpen, EmbeddedRdbSuperblock, RdbFileError,
    RdbFileResult, DEFAULT_FORMAT_VERSION, EMBEDDED_RDB_MANIFEST_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_SIZE,
};
pub use file_format::{
    clear_paged_page_checksum, database_header_freelist_head, database_header_magic_matches,
    database_header_page_count, database_header_page_size, decode_database_header,
    decode_paged_dwb_frame, decode_paged_encryption_header, decode_paged_page_header,
    encode_database_header, encode_paged_dwb_frame, encode_paged_encryption_header,
    encode_paged_page_header, init_database_header_page, paged_cell_bytes, paged_cell_key_value,
    paged_cell_len, paged_cell_pointer, paged_cell_pointer_is_valid, paged_cell_pointer_offset,
    paged_cell_total_len, paged_encryption_header_bytes, paged_encryption_marker_present,
    paged_page_cell_count, paged_page_checksum, paged_page_free_end, paged_page_free_start,
    paged_page_id, paged_page_lsn, paged_page_parent_id, paged_page_right_child, paged_page_type,
    set_database_header_freelist_head, set_database_header_page_count, set_database_header_version,
    set_paged_cell_pointer, set_paged_page_cell_count, set_paged_page_checksum,
    set_paged_page_free_end, set_paged_page_free_start, set_paged_page_lsn,
    set_paged_page_parent_id, set_paged_page_right_child, write_paged_cell,
    write_paged_encryption_marker_and_header, DatabaseHeader, DatabaseHeaderError, PagedDwbEntry,
    PagedDwbFrameError, PagedEncryptionHeader, PagedPageHeader, PhysicalFileHeader,
    BLOOM_SEGMENT_MAGIC, COLUMN_BLOCK_MAGIC, COLUMN_BLOCK_VERSION_V1, DWB_MAGIC,
    PAGED_CELL_HEADER_SIZE, PAGED_CELL_POINTER_SIZE, PAGED_DWB_ENTRY_HEADER_SIZE,
    PAGED_DWB_ENTRY_SIZE, PAGED_DWB_HEADER_SIZE, PAGED_ENCRYPTION_HEADER_SIZE,
    PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE, PAGED_ENCRYPTION_KEY_CHECK_PLAINTEXT_SIZE,
    PAGED_ENCRYPTION_MARKER, PAGED_ENCRYPTION_MARKER_OFFSET, PAGED_ENCRYPTION_SALT_SIZE,
    PAGED_PAGE_HEADER_SIZE, PAGED_PAGE_SIZE, PAGE_FILE_MAGIC, PAGE_FILE_VERSION,
    VECTOR_BTREE_FORMAT_VERSION, VECTOR_BTREE_FORMAT_VERSION_V1, VECTOR_BTREE_FORMAT_VERSION_V2,
};
pub use graph_label_registry::{
    decode_graph_label_registry_frame, encode_graph_label_registry_frame, GraphLabelRegistryEntry,
    GraphLabelRegistryFrameError, GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN,
};
pub use graph_record::{
    decode_graph_edge_record_v1, decode_graph_edge_record_v2, decode_graph_node_record_v1,
    decode_graph_node_record_v2, decode_graph_table_ref, encode_graph_edge_record_v2,
    encode_graph_node_record_v2, encode_graph_table_ref, graph_edge_record_v2_encoded_size,
    graph_node_record_v2_encoded_size, GraphEdgeRecord, GraphNodeRecord, GraphTableRef,
    GraphVectorRef, LegacyGraphEdgeRecord, LegacyGraphNodeRecord, GRAPH_EDGE_HEADER_SIZE,
    GRAPH_EDGE_HEADER_SIZE_V1, GRAPH_MAX_ID_SIZE, GRAPH_MAX_LABEL_SIZE,
    GRAPH_NODE_FLAG_HAS_TABLE_REF, GRAPH_NODE_FLAG_HAS_VECTOR_REF, GRAPH_NODE_HEADER_SIZE,
    GRAPH_NODE_HEADER_SIZE_V1, GRAPH_TABLE_REF_SIZE, GRAPH_VECTOR_REF_HEADER_SIZE,
};
pub use graph_store::{
    decode_graph_store_frame, encode_graph_store_frame, GraphStoreFrame, GraphStoreFrameError,
    GRAPH_STORE_HEADER_LEN, GRAPH_STORE_MAGIC, GRAPH_STORE_PAGE_COUNT_BYTES,
    GRAPH_STORE_REGISTRY_LEN_BYTES, GRAPH_STORE_VERSION_V1, GRAPH_STORE_VERSION_V2,
};
pub use graph_table_index::{
    decode_graph_table_index_frame, encode_graph_table_index_frame, GraphTableIndexEntry,
    GraphTableIndexFrameError, GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN, GRAPH_TABLE_INDEX_HEADER_LEN,
    GRAPH_TABLE_INDEX_MAX_NODE_ID_LEN,
};
pub use layout::{
    audit_log_rotated_compressed_path, audit_log_rotated_plain_path, data_file_name,
    default_database_path, default_service_database_path, engine_wal_path, legacy_audit_log_path,
    legacy_logical_slots_path, legacy_logical_slots_temp_path, legacy_slow_query_log_path,
    local_cas_lock_path, local_upload_temp_path, logical_wal_path, logical_wal_path_in,
    logical_wal_temp_path, pager_dwb_path, pager_dwb_shadow_path, pager_header_path,
    pager_header_shadow_path, pager_legacy_wal_path, pager_meta_path, pager_meta_shadow_path,
    parse_audit_log_rotated_timestamp, physical_export_data_path, physical_metadata_binary_path,
    physical_metadata_journal_path, physical_metadata_journal_prefix, physical_metadata_json_path,
    primary_replica_root, primary_wal_segment_file_name, rebootstrap_intent_log_path,
    rebootstrap_pending_path, rebootstrap_previous_path, rebootstrap_ready_marker_path,
    rebootstrap_staging_root, relay_segment_relative_path, serverless_cache_root,
    serverless_namespace, serverless_root, shm_path, sibling_path, sidecar_file_name,
    store_commit_coord_temp_wal_file_name, store_commit_coord_temp_wal_path, support_dir_for,
    temp_path, temp_path_in, unified_wal_path, unified_wal_path_in, LayoutOverrides, LayoutToggles,
    LogDestination, LogRoutingOverrides, StorageLayout, TieredLayoutPaths,
    DEFAULT_DATABASE_FILE_NAME, DEFAULT_SERVICE_DATABASE_PATH,
};
pub use local_backend::{local_backend_atomic_upload, local_backend_download};
pub use logical_wal::{
    build_logical_wal_seek_index, encode_logical_wal_v2_for_compat, encode_logical_wal_v3,
    read_and_repair_logical_wal_entries, read_logical_wal_entries_from,
    rewrite_logical_wal_entries, LogicalWalEntry, LOGICAL_WAL_CRC_LEN,
    LOGICAL_WAL_SEEK_INDEX_INTERVAL, LOGICAL_WAL_SPOOL_MAGIC, LOGICAL_WAL_SPOOL_VERSION_CURRENT,
    LOGICAL_WAL_SPOOL_VERSION_V1, LOGICAL_WAL_SPOOL_VERSION_V2, LOGICAL_WAL_SPOOL_VERSION_V3,
    LOGICAL_WAL_V3_HEADER_LEN,
};
pub use native_store::{
    append_native_store_crc32_footer, decode_native_blob_page, decode_native_catalog_summary_page,
    decode_native_collection_roots_page, decode_native_dump_collection_header,
    decode_native_dump_count, decode_native_dump_cross_ref, decode_native_dump_entity_record,
    decode_native_entity_record_frame, decode_native_len_prefixed_bytes,
    decode_native_len_prefixed_string, decode_native_manifest_summary_page,
    decode_native_metadata_overflow_continuation_header, decode_native_metadata_overflow_header,
    decode_native_metadata_state_summary_page, decode_native_paged_collection_root,
    decode_native_paged_cross_ref, decode_native_paged_metadata_header,
    decode_native_recovery_summary_page, decode_native_registry_summary_page,
    decode_native_store_header, decode_native_vector_artifact_store_page, encode_native_blob_page,
    encode_native_catalog_summary_page, encode_native_collection_roots_page,
    encode_native_dump_collection_header, encode_native_dump_count, encode_native_dump_cross_ref,
    encode_native_dump_entity_record, encode_native_entity_record_frame,
    encode_native_len_prefixed_bytes, encode_native_len_prefixed_str,
    encode_native_manifest_summary_page, encode_native_metadata_overflow_continuation_header,
    encode_native_metadata_overflow_header, encode_native_metadata_state_summary_page,
    encode_native_paged_collection_root, encode_native_paged_cross_ref,
    encode_native_paged_metadata_header, encode_native_recovery_summary_page,
    encode_native_registry_summary_page, encode_native_store_header,
    encode_native_vector_artifact_store_page, is_supported_store_version,
    native_blob_chunk_capacity, native_store_magic_matches, native_store_page_checksum,
    verify_native_store_crc32_footer, write_native_store_bytes_atomically,
    NativeCatalogCollectionSummary, NativeCatalogSummary, NativeDumpCollectionHeader,
    NativeDumpCrossRef, NativeEntityRecordFrame, NativeExportSummary, NativeManifestEntrySummary,
    NativeManifestSummary, NativeMetadataOverflowContinuationHeader, NativeMetadataOverflowHeader,
    NativeMetadataStateSummary, NativePagedCollectionRoot, NativePagedCrossRef,
    NativePagedMetadataHeader, NativeRecoverySummary, NativeRegistryIndexSummary,
    NativeRegistryJobSummary, NativeRegistryProjectionSummary, NativeRegistrySummary,
    NativeSnapshotSummary, NativeVectorArtifactPageSummary, NativeVectorArtifactSummary,
    ENTITY_RECORD_MAGIC, METADATA_HEADER_BYTES, METADATA_MAGIC,
    METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES, METADATA_OVERFLOW_HEADER_BYTES,
    METADATA_OVERFLOW_MAGIC, NATIVE_BLOB_MAGIC, NATIVE_BLOB_PAGE_HEADER_BYTES,
    NATIVE_CATALOG_MAGIC, NATIVE_COLLECTION_ROOTS_MAGIC, NATIVE_MANIFEST_MAGIC,
    NATIVE_MANIFEST_SAMPLE_LIMIT, NATIVE_METADATA_STATE_MAGIC, NATIVE_RECOVERY_MAGIC,
    NATIVE_REGISTRY_MAGIC, NATIVE_VECTOR_ARTIFACT_MAGIC, STORE_MAGIC, STORE_VERSION_CURRENT,
    STORE_VERSION_V1, STORE_VERSION_V2, STORE_VERSION_V3, STORE_VERSION_V4, STORE_VERSION_V5,
    STORE_VERSION_V6, STORE_VERSION_V7, STORE_VERSION_V8, STORE_VERSION_V9,
};
pub use operational_manifest::OperationalManifest;
pub use physical_metadata::{
    copy_physical_export_data_file, copy_physical_metadata_binary_to_journal,
    decode_persisted_physical_hypertable_chunk_json, decode_persisted_physical_hypertable_json,
    decode_persisted_physical_index_state_json, decode_physical_analytical_storage_json,
    decode_physical_analytics_job_json, decode_physical_analytics_view_descriptor_json,
    decode_physical_block_reference_json, decode_physical_catalog_snapshot_json,
    decode_physical_collection_contract_json, decode_physical_declared_column_contract_json,
    decode_physical_export_descriptor_json, decode_physical_graph_projection_json,
    decode_physical_manifest_event_json, decode_physical_manifest_pointers_json,
    decode_physical_metadata_document, decode_physical_metadata_document_root_json,
    decode_physical_schema_manifest_json, decode_physical_snapshot_descriptor_json,
    decode_physical_subscription_descriptor_json, decode_physical_superblock_json,
    decode_physical_tree_definition_json, encode_persisted_physical_hypertable_chunk_json,
    encode_persisted_physical_hypertable_json, encode_persisted_physical_index_state_json,
    encode_physical_analytical_storage_json, encode_physical_analytics_job_json,
    encode_physical_analytics_view_descriptor_json, encode_physical_block_reference_json,
    encode_physical_catalog_snapshot_json, encode_physical_collection_contract_json,
    encode_physical_declared_column_contract_json, encode_physical_export_descriptor_json,
    encode_physical_graph_projection_json, encode_physical_manifest_event_json,
    encode_physical_manifest_pointers_json, encode_physical_metadata_binary_document,
    encode_physical_metadata_document_root_json, encode_physical_metadata_json_document,
    encode_physical_schema_manifest_json, encode_physical_snapshot_descriptor_json,
    encode_physical_subscription_descriptor_json, encode_physical_superblock_json,
    encode_physical_tree_definition_json, list_physical_metadata_journal_paths,
    prune_physical_metadata_journal_paths, read_physical_metadata_document,
    write_physical_metadata_binary_document, write_physical_metadata_json_document, BlockReference,
    ExportDescriptor, ManifestEvent, ManifestEventKind, ManifestPointers,
    PersistedPhysicalHypertable, PersistedPhysicalHypertableChunk, PersistedPhysicalIndexState,
    PhysicalAnalyticalStorageConfig, PhysicalAnalyticsJob, PhysicalAnalyticsViewDescriptor,
    PhysicalCatalogCollectionStats, PhysicalCatalogSnapshot, PhysicalCollectionContract,
    PhysicalDeclaredColumnContract, PhysicalGraphProjection, PhysicalMetadataDocumentEnvelope,
    PhysicalPageLocation, PhysicalSchemaManifest, PhysicalSchemaOptions, PhysicalSqlTypeName,
    PhysicalSubscriptionDescriptor, PhysicalTreeDefinition, PhysicalTypeModifier,
    SnapshotDescriptor, SuperblockHeader, DEFAULT_PHYSICAL_FORMAT_VERSION,
    DEFAULT_SUPERBLOCK_COPIES, PHYSICAL_METADATA_PROTOCOL_VERSION,
};
pub use physical_metadata_policy::{
    fold_dwb_into_wal_enabled, fold_pager_meta_enabled, meta_json_sidecar_enabled,
    seqn_journal_enabled, seqn_journal_retention, set_fold_dwb_into_wal_enabled,
    set_fold_pager_meta_enabled, set_meta_json_sidecar_enabled, set_seqn_journal_enabled,
    set_seqn_journal_retention, DEFAULT_METADATA_JOURNAL_RETENTION,
    OPT_IN_METADATA_JOURNAL_RETENTION,
};

pub use primary_replica::{
    cleanup_rebootstrap_artifacts, decode_rebootstrap_ready_marker_json,
    discard_ready_rebootstrap_marker, encode_rebootstrap_ready_marker_json,
    promote_rebootstrap_pending_database, read_rebootstrap_ready_marker,
    write_rebootstrap_ready_marker, BaseBackupChunkRef, BaseBackupPlan,
    PrimaryReplicaBaseBackupManifest, PrimaryReplicaFilePlan, PrimaryReplicaWalRecord,
    PrimaryReplicaWalSegment, PromotionCandidate, RejoinDecision, RelayLogSegmentRef, ReplicaAck,
    ReplicaCatchupMode, ReplicaRebootstrapReadyMarker, ReplicaRelayLogManifest,
    ReplicaRelayLogRecord, ReplicaRelayLogSegment, ReplicationDurability, ReplicationSlot,
    ReplicationSlotCatalog, ReplicationSlotInvalidationCause, TimelineHistory,
    TimelineHistoryEntry, TimelineId, WalPruneResult, WalRetentionPlan, WalRetentionPolicy,
};
pub use profile::{FileArtifactKind, FileProfile};
pub use serverless::{
    decode_serverless_writer_lease_json, encode_serverless_writer_lease_json,
    serverless_writer_lease_key, serverless_writer_lease_temp_path, ServerlessBootIndex,
    ServerlessBootIndexEntry, ServerlessBootPlan, ServerlessCacheEntry,
    ServerlessCacheEvictionPlan, ServerlessCachePolicy, ServerlessContentHash,
    ServerlessExtentIndex, ServerlessExtentRef, ServerlessFilePlan, ServerlessGenerationPointer,
    ServerlessHydratedRange, ServerlessHydrationPlan, ServerlessHydrationRequest,
    ServerlessLocalCache, ServerlessManifest, ServerlessManifestEntry, ServerlessPackKind,
    ServerlessSecondaryIndex, ServerlessSecondaryIndexEntry, ServerlessWriterLease,
    ServerlessWriterLeaseTempFile, SERVERLESS_WRITER_LEASE_DEFAULT_TERM,
};
pub use shm::{
    initialize_shm_file, read_shm_header_from_file, write_shm_header_to_file, ShmHeader,
    SHM_FILE_SIZE, SHM_HEADER_SIZE, SHM_MAGIC, SHM_VERSION,
};
pub use spill::{
    decode_spill_file_frame, default_spill_dir, encode_spill_file_frame, is_spill_file_path,
    spill_file_name, SpillFileFrameError, DEFAULT_SPILL_DIR_NAME, SPILL_FILE_EXTENSION,
    SPILL_FILE_HEADER_LEN, SPILL_FILE_MAGIC, SPILL_FILE_VERSION_V1, SPILL_FILE_VERSION_V2,
};
pub use store_wal::{
    decode_store_wal_action_frame, encode_store_wal_action_frame, StoreWalActionFrame,
    STORE_WAL_ACTION_VERSION,
};
pub use transaction_wal::{
    decode_transaction_wal_entry_payload, decode_transaction_wal_record_frame,
    encode_transaction_wal_entry_payload, encode_transaction_wal_record_frame,
    transaction_wal_record_encoded_len, TransactionWalEntryPayload, TransactionWalRecordFrame,
    TRANSACTION_WAL_RECORD_CHECKSUM_LEN, TRANSACTION_WAL_RECORD_HEADER_LEN,
    TRANSACTION_WAL_RECORD_LEN_LEN, TRANSACTION_WAL_RECORD_MIN_LEN,
};
pub use turboquant_snapshot::{
    read_turboquant_snapshot, write_turboquant_snapshot, TurboQuantSnapshotError,
    TurboQuantSnapshotPayload, TURBOQUANT_SNAPSHOT_HEADER_BYTES,
};
pub use wal_header::{
    decode_wal_file_header, encode_wal_file_header, next_main_wal_segment_boundary, WalFileHeader,
    MAIN_WAL_SEGMENT_BYTES, WAL_FILE_HEADER_BYTES, WAL_FILE_MAGIC, WAL_FILE_VERSION,
    WAL_FILE_VERSION_V2,
};
pub use wal_record::{
    decode_main_wal_record_frame, encode_main_wal_record_frame, encode_main_wal_record_frame_into,
    MainWalCompression, MainWalRecordFrame, MainWalRecordType, MAIN_WAL_DEFAULT_COMPRESS_THRESHOLD,
};
pub use zone_map::{
    read_zone_map_sidecar, write_zone_map_sidecar, PersistedZone, ZoneMapPersistError,
};
