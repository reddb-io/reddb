use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/reddb-file has workspace root two levels up")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("read {}: {err}", path.as_ref().display()))
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries =
            fs::read_dir(&path).unwrap_or_else(|err| panic!("read_dir {}: {err}", path.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn server_uses_reddb_file_for_unified_wal_paths() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/storage/unified/store/commit.rs",
        "crates/reddb-server/src/storage/unified/store/impl_pages.rs",
        "crates/reddb-server/src/runtime/impl_dml.rs",
        "crates/reddb-server/src/storage/layout.rs",
        "crates/reddb-server/src/storage/engine/database.rs",
        "crates/reddb-server/src/storage/engine/pager.rs",
        "crates/reddb-server/src/physical/shm.rs",
        "crates/reddb-server/src/storage/unified/store/impl_file.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "wal_path_for_db",
            "rb_commit_coord_",
            "with_extension(\"rdb-uwal\")",
            "with_extension(\"rdb-wal\")",
            "with_extension(\"rdb-tmp\")",
            "set_extension(\"wal\")",
            "with_extension(\"shm\")",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} must call reddb_file::layout instead of rebuilding {forbidden:?}"
            );
        }
    }
}

#[test]
fn server_does_not_own_unified_store_wal_action_frame() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/unified/store/commit.rs"));

    for forbidden in [
        "const STORE_WAL_VERSION",
        "store wal action too short",
        "unsupported store wal version",
        "unsupported store wal action tag",
        "bulk upsert wal action: missing record count",
        "refresh collection wal action: missing record count",
        "fn write_string(",
        "fn write_bytes(",
        "fn read_string(",
        "fn read_bytes(",
    ] {
        assert!(
            !text.contains(forbidden),
            "UnifiedStore WAL action frame belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::encode_store_wal_action_frame",
        "reddb_file::decode_store_wal_action_frame",
        "reddb_file::StoreWalActionFrame",
    ] {
        assert!(
            text.contains(required),
            "UnifiedStore WAL action frame should route through {required}"
        );
    }
}

#[test]
fn server_does_not_own_main_wal_file_header() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/storage/wal/record.rs",
        "crates/reddb-server/src/storage/wal/reader.rs",
        "crates/reddb-server/src/storage/wal/writer.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "pub const WAL_MAGIC",
            "pub const WAL_VERSION",
            "pub const WAL_VERSION_V2",
            "extend_from_slice(WAL_MAGIC)",
            "push(WAL_VERSION)",
            "Invalid WAL magic bytes",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} must route WAL file header contracts through reddb-file, found {forbidden:?}"
            );
        }
    }

    let reader = read(root.join("crates/reddb-server/src/storage/wal/reader.rs"));
    assert!(
        reader.contains("reddb_file::decode_wal_file_header"),
        "WAL reader should validate file headers through reddb-file"
    );
    let writer = read(root.join("crates/reddb-server/src/storage/wal/writer.rs"));
    assert!(
        writer.contains("reddb_file::encode_wal_file_header"),
        "WAL writer should encode file headers through reddb-file"
    );
}

#[test]
fn server_does_not_own_main_wal_record_frame() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/record.rs"));
    let non_test = text
        .split("#[cfg(test)]")
        .next()
        .expect("record.rs has non-test source");

    for forbidden in [
        "pub enum RecordType",
        "pub enum Compression",
        "COMPRESS_THRESHOLD",
        "crc32_update",
        "fn crc32",
        "WAL record checksum mismatch",
        "Unknown WAL compression algorithm",
        "WAL zstd decompress failed",
        "Invalid record type",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "main WAL record frame belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::MainWalRecordType",
        "decode_main_wal_record_frame",
        "encode_main_wal_record_frame_into",
        "MainWalRecordFrame",
    ] {
        assert!(
            non_test.contains(required),
            "main WAL record frame should route through {required}"
        );
    }
}

#[test]
fn server_source_does_not_embed_owned_file_suffixes() {
    let root = repo_root();
    for path in rust_files_under(&root.join("crates/reddb-server/src")) {
        let text = read(&path);
        for forbidden in [
            ".redwal",
            ".rdb-dwb",
            ".rdb-meta",
            ".rdb-hdr",
            "rdb-wal",
            "with_extension(\"rdb-wal\")",
            "with_extension(\"rdb-hdr\")",
            "with_extension(\"rdb-meta\")",
            "with_extension(\"rdb-dwb\")",
            "set_extension(\"wal\")",
            "with_extension(\"shm\")",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} must route file suffix contracts through reddb-file, found {forbidden:?}",
                path.display()
            );
        }
    }
}

#[test]
fn server_does_not_redeclare_tiered_layout_contracts() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/storage/layout.rs"));
    let file = read(root.join("crates/reddb-file/src/layout.rs"));

    for forbidden in [
        "pub enum StorageLayout",
        "pub struct LayoutOverrides",
        "pub enum LogDestination",
        "pub struct LogRoutingOverrides",
        "pub struct LayoutToggles",
        "pub struct TieredLayoutPaths",
        "support_dir.join(\"snapshots\")",
        "support_dir.join(\"indexes\")",
        "support_dir.join(\"cache\")",
        "support_dir.join(\"blobs\")",
        "support_dir.join(\"metrics\")",
        "support_dir.join(\"logs\")",
    ] {
        assert!(
            !server.contains(forbidden),
            "tiered layout file contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub enum StorageLayout",
        "pub struct LayoutOverrides",
        "pub enum LogDestination",
        "pub struct TieredLayoutPaths",
        "support_dir.join(\"snapshots\")",
        "support_dir.join(\"logs\")",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own tiered layout contract {required}"
        );
    }

    assert!(
        server.contains("pub use reddb_file::{"),
        "server storage::layout should be a compatibility reexport only"
    );
}

#[test]
fn server_does_not_redeclare_physical_metadata_core_contracts() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/physical.rs"));
    let file = read(root.join("crates/reddb-file/src/physical_metadata.rs"));

    for forbidden in [
        "pub struct BlockReference",
        "pub struct ManifestPointers",
        "pub struct SuperblockHeader",
        "pub enum ManifestEventKind",
        "pub struct ManifestEvent",
        "pub struct SnapshotDescriptor",
        "pub struct ExportDescriptor",
        "pub struct PhysicalGraphProjection",
        "pub struct PhysicalAnalyticsJob",
        "pub struct PhysicalTreeDefinition",
        "pub const DEFAULT_SUPERBLOCK_COPIES",
    ] {
        assert!(
            !server.contains(forbidden),
            "physical metadata persisted core contract belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub struct BlockReference",
        "pub struct ManifestPointers",
        "pub struct SuperblockHeader",
        "pub enum ManifestEventKind",
        "pub struct ManifestEvent",
        "pub struct SnapshotDescriptor",
        "pub struct ExportDescriptor",
        "pub struct PhysicalGraphProjection",
        "pub struct PhysicalAnalyticsJob",
        "pub struct PhysicalTreeDefinition",
        "pub const DEFAULT_SUPERBLOCK_COPIES",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own physical metadata core contract {required}"
        );
    }

    assert!(
        server.contains("pub use reddb_file::{")
            && server.contains("BlockReference")
            && server.contains("SuperblockHeader")
            && server.contains("SnapshotDescriptor")
            && server.contains("ExportDescriptor")
            && server.contains("PhysicalGraphProjection")
            && server.contains("PhysicalAnalyticsJob")
            && server.contains("PhysicalTreeDefinition"),
        "server physical module should compatibility-reexport physical metadata core contracts"
    );
}

#[test]
fn server_does_not_redeclare_native_store_file_contracts() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/storage/unified/store.rs"));
    let native_a =
        read(root.join("crates/reddb-server/src/storage/unified/store/impl_native_a.rs"));
    let file = read(root.join("crates/reddb-file/src/native_store.rs"));
    let server_store_files = rust_files_under(
        &root
            .join("crates/reddb-server/src/storage/unified/store")
            .to_path_buf(),
    );

    for forbidden in [
        "const STORE_MAGIC",
        "const STORE_VERSION_V1",
        "const STORE_VERSION_V9",
        "const METADATA_MAGIC",
        "const NATIVE_COLLECTION_ROOTS_MAGIC",
        "const NATIVE_MANIFEST_MAGIC",
        "const NATIVE_REGISTRY_MAGIC",
        "const NATIVE_RECOVERY_MAGIC",
        "const NATIVE_CATALOG_MAGIC",
        "const NATIVE_METADATA_STATE_MAGIC",
        "const NATIVE_VECTOR_ARTIFACT_MAGIC",
        "const NATIVE_BLOB_MAGIC",
        "const ENTITY_RECORD_MAGIC",
        "const METADATA_OVERFLOW_MAGIC",
        "pub struct NativeManifestEntrySummary",
        "pub struct NativeManifestSummary",
        "pub struct NativeRegistryIndexSummary",
        "pub struct NativeRegistrySummary",
        "pub struct NativeRecoverySummary",
        "pub struct NativeCatalogSummary",
        "pub struct NativeMetadataStateSummary",
        "pub struct NativeVectorArtifactPageSummary",
    ] {
        assert!(
            !server.contains(forbidden),
            "native store persisted contract belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub const STORE_MAGIC",
        "pub const STORE_VERSION_CURRENT",
        "pub const METADATA_MAGIC",
        "pub const NATIVE_COLLECTION_ROOTS_MAGIC",
        "pub const NATIVE_MANIFEST_MAGIC",
        "pub const NATIVE_REGISTRY_MAGIC",
        "pub const NATIVE_RECOVERY_MAGIC",
        "pub const NATIVE_CATALOG_MAGIC",
        "pub const NATIVE_METADATA_STATE_MAGIC",
        "pub const NATIVE_VECTOR_ARTIFACT_MAGIC",
        "pub const NATIVE_BLOB_MAGIC",
        "pub const ENTITY_RECORD_MAGIC",
        "pub const METADATA_OVERFLOW_MAGIC",
        "pub struct NativeManifestEntrySummary",
        "pub struct NativeManifestSummary",
        "pub struct NativeRegistryIndexSummary",
        "pub struct NativeRegistrySummary",
        "pub struct NativeRecoverySummary",
        "pub struct NativeCatalogSummary",
        "pub struct NativeMetadataStateSummary",
        "pub struct NativeVectorArtifactPageSummary",
        "pub fn is_supported_store_version",
        "pub fn encode_native_collection_roots_page",
        "pub fn decode_native_collection_roots_page",
        "pub fn encode_native_manifest_summary_page",
        "pub fn decode_native_manifest_summary_page",
        "pub fn encode_native_registry_summary_page",
        "pub fn decode_native_registry_summary_page",
        "pub fn encode_native_recovery_summary_page",
        "pub fn decode_native_recovery_summary_page",
        "pub fn encode_native_catalog_summary_page",
        "pub fn decode_native_catalog_summary_page",
        "pub fn encode_native_metadata_state_summary_page",
        "pub fn decode_native_metadata_state_summary_page",
        "pub fn encode_native_blob_page",
        "pub fn decode_native_blob_page",
        "pub fn encode_native_vector_artifact_store_page",
        "pub fn decode_native_vector_artifact_store_page",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own native store file contract {required}"
        );
    }

    assert!(
        server.contains("pub use reddb_file::{")
            && server.contains("NativeManifestSummary")
            && server.contains("STORE_VERSION_CURRENT")
            && server.contains("is_supported_store_version"),
        "server store module should compatibility-reexport native store file contracts"
    );

    for forbidden in [
        "extend_from_slice(NATIVE_COLLECTION_ROOTS_MAGIC)",
        "extend_from_slice(NATIVE_MANIFEST_MAGIC)",
        "invalid native collection roots page",
        "invalid native manifest summary page",
        "truncated native manifest snapshot_max",
    ] {
        assert!(
            !native_a.contains(forbidden),
            "native collection roots/manifest codecs belong in reddb-file, found {forbidden:?}"
        );
    }

    for path in server_store_files {
        let text = read(&path);
        for forbidden in [
            "const ENTITY_RECORD_MAGIC",
            "const METADATA_OVERFLOW_MAGIC",
            "extend_from_slice(NATIVE_",
            "invalid native registry summary page",
            "invalid native recovery summary page",
            "invalid native catalog summary page",
            "invalid native metadata state page",
            "invalid native blob page",
            "invalid native vector artifact store page",
            "truncated native string",
            "truncated native registry",
            "truncated native analytics",
            "truncated native projection",
            "truncated native export",
            "truncated native metadata",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} must delegate native persisted codecs to reddb-file, found {forbidden:?}",
                path.display()
            );
        }
    }
}

#[test]
fn server_does_not_redeclare_core_file_format_constants() {
    let root = repo_root();
    let page = read(root.join("crates/reddb-server/src/storage/engine/page.rs"));
    let page_impl = read(root.join("crates/reddb-server/src/storage/engine/page/impl.rs"));
    let pager = read(root.join("crates/reddb-server/src/storage/engine/pager.rs"));
    let pager_impl = read(root.join("crates/reddb-server/src/storage/engine/pager/impl.rs"));
    let column_block = read(root.join("crates/reddb-server/src/storage/unified/column_block.rs"));
    let bloom_segment = read(root.join("crates/reddb-server/src/storage/index/bloom_segment.rs"));
    let vector_btree =
        read(root.join("crates/reddb-server/src/storage/engine/vector_btree/page_format.rs"));
    let vector_value_codec =
        read(root.join("crates/reddb-server/src/storage/engine/vector_btree/value_codec.rs"));
    let btree_value_layout =
        read(root.join("crates/reddb-server/src/storage/engine/btree/value_layout.rs"));
    let graph_store = read(root.join("crates/reddb-server/src/storage/engine/graph_store.rs"));
    let graph_store_impl =
        read(root.join("crates/reddb-server/src/storage/engine/graph_store/impl.rs"));
    let graph_table_index =
        read(root.join("crates/reddb-server/src/storage/engine/graph_table_index.rs"));
    let graph_label_registry =
        read(root.join("crates/reddb-server/src/storage/engine/graph_store/label_registry.rs"));
    let physical = read(root.join("crates/reddb-server/src/physical.rs"));
    let file_format = read(root.join("crates/reddb-file/src/file_format.rs"));
    let file_bloom_segment = read(root.join("crates/reddb-file/src/bloom_segment.rs"));
    let file_column_block = read(root.join("crates/reddb-file/src/column_block.rs"));
    let file_graph_label_registry =
        read(root.join("crates/reddb-file/src/graph_label_registry.rs"));
    let file_graph_record = read(root.join("crates/reddb-file/src/graph_record.rs"));
    let file_graph_store = read(root.join("crates/reddb-file/src/graph_store.rs"));
    let file_graph_table_index = read(root.join("crates/reddb-file/src/graph_table_index.rs"));
    let file_vector_btree = read(root.join("crates/reddb-file/src/vector_btree_page_format.rs"));
    let file_vector_value_codec = read(root.join("crates/reddb-file/src/vector_value_codec.rs"));
    let file_btree_value_layout = read(root.join("crates/reddb-file/src/btree_value_layout.rs"));
    let physical_metadata = read(root.join("crates/reddb-file/src/physical_metadata.rs"));
    let column_block_non_test = column_block
        .split("#[cfg(test)]")
        .next()
        .expect("column_block.rs has non-test source");
    let graph_label_registry_non_test = graph_label_registry
        .split("#[cfg(test)]")
        .next()
        .expect("label_registry.rs has non-test source");
    let graph_store_non_test = graph_store
        .split("#[cfg(test)]")
        .next()
        .expect("graph_store.rs has non-test source");
    let graph_store_impl_non_test = graph_store_impl
        .split("#[cfg(test)]")
        .next()
        .expect("graph_store/impl.rs has non-test source");
    let graph_table_index_non_test = graph_table_index
        .split("#[cfg(test)]")
        .next()
        .expect("graph_table_index.rs has non-test source");

    for (label, text) in [
        ("storage/engine/page.rs", page.as_str()),
        ("storage/engine/page/impl.rs", page_impl.as_str()),
        ("storage/engine/pager.rs", pager.as_str()),
        ("storage/engine/pager/impl.rs", pager_impl.as_str()),
        ("storage/unified/column_block.rs", column_block.as_str()),
        ("storage/index/bloom_segment.rs", bloom_segment.as_str()),
        (
            "storage/engine/vector_btree/page_format.rs",
            vector_btree.as_str(),
        ),
        (
            "storage/engine/vector_btree/value_codec.rs",
            vector_value_codec.as_str(),
        ),
        (
            "storage/engine/btree/value_layout.rs",
            btree_value_layout.as_str(),
        ),
        ("physical.rs", physical.as_str()),
    ] {
        for forbidden in [
            "pub const MAGIC_BYTES",
            "pub const DB_VERSION",
            "const DWB_MAGIC",
            "pub const COLUMN_BLOCK_MAGIC",
            "pub const COLUMN_BLOCK_VERSION_V1",
            "const BLOOM_SEGMENT_MAGIC",
            "pub const FORMAT_VERSION_V1",
            "pub const FORMAT_VERSION_V2",
            "pub const FORMAT_VERSION: u16",
            "pub const PHYSICAL_METADATA_PROTOCOL_VERSION",
            "pub struct DatabaseHeader",
            "pub struct PhysicalFileHeader",
            "pub struct PagedPageHeader",
            "pub struct PagedEncryptionHeader",
            "fn decode_database_header",
            "fn encode_database_header",
            "fn decode_paged_page_header",
            "fn encode_paged_page_header",
            "self.data[20..28].copy_from_slice(&lsn.to_le_bytes())",
            "self.data[28..32].copy_from_slice",
            "HEADER_SIZE + index * 2",
            "Cell format: [key_len",
            "cell_offset + 6",
            "u32::from_le_bytes([cell[2], cell[3], cell[4], cell[5]])",
            "page.data[HEADER_SIZE..HEADER_SIZE + 4]",
            "self.data[HEADER_SIZE + 12",
            "self.data[HEADER_SIZE + 16",
            "HEADER_SIZE + 4..HEADER_SIZE + 8",
            "DB_VERSION + 1",
            "Build DWB content:",
            "buf[8..12].copy_from_slice",
            "u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]])",
            "u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]])",
            "const ENCRYPTION_MARKER_OFFSET",
            "const ENCRYPTION_MARKER",
            "b\"RDBE\"",
            "HEADER_SIZE + 32",
            "buf[2..4].copy_from_slice(&self.cell_count.to_le_bytes())",
            "u16::from_le_bytes([buf[2], buf[3]])",
            "[0x52, 0x44, 0x44, 0x42]",
            "[0x52, 0x44, 0x44, 0x57]",
            "*b\"RDCC\"",
            "0xBF",
            "\"reddb-physical-v1\"",
        ] {
            assert!(
                !text.contains(forbidden),
                "{label} must import persisted file constants from reddb-file, found {forbidden:?}"
            );
        }
    }

    for required in [
        "pub const PAGE_FILE_MAGIC",
        "pub const PAGE_FILE_VERSION",
        "pub const DWB_MAGIC",
        "pub const COLUMN_BLOCK_MAGIC",
        "pub const COLUMN_BLOCK_VERSION_V1",
        "pub const BLOOM_SEGMENT_MAGIC",
        "pub const VECTOR_BTREE_FORMAT_VERSION_V1",
        "pub const VECTOR_BTREE_FORMAT_VERSION_V2",
        "pub const VECTOR_BTREE_FORMAT_VERSION",
        "pub struct DatabaseHeader",
        "pub struct PhysicalFileHeader",
        "pub struct PagedPageHeader",
        "pub struct PagedEncryptionHeader",
        "pub fn decode_database_header",
        "pub fn encode_database_header",
        "pub fn database_header_magic_matches",
        "pub fn decode_paged_page_header",
        "pub fn encode_paged_page_header",
        "pub fn paged_page_lsn",
        "pub fn set_paged_page_lsn",
        "pub fn paged_page_checksum",
        "pub fn clear_paged_page_checksum",
        "pub fn paged_cell_pointer",
        "pub fn set_paged_cell_pointer",
        "pub fn paged_cell_bytes",
        "pub fn paged_cell_key_value",
        "pub fn write_paged_cell",
        "pub fn init_database_header_page",
        "pub fn set_database_header_version",
        "pub fn database_header_page_count",
        "pub fn set_database_header_page_count",
        "pub fn database_header_freelist_head",
        "pub fn set_database_header_freelist_head",
        "pub fn database_header_page_size",
        "pub fn encode_paged_dwb_frame",
        "pub fn decode_paged_dwb_frame",
        "pub fn decode_paged_encryption_header",
        "pub fn encode_paged_encryption_header",
        "pub fn paged_encryption_marker_present",
        "pub fn write_paged_encryption_marker_and_header",
    ] {
        assert!(
            file_format.contains(required),
            "reddb-file should own core file-format constant {required}"
        );
    }
    assert!(
        physical_metadata.contains("pub const PHYSICAL_METADATA_PROTOCOL_VERSION"),
        "reddb-file should own physical metadata protocol version"
    );
    for required in [
        "pub struct BloomSegmentFrame",
        "pub enum BloomSegmentFrameError",
        "pub fn encode_bloom_segment_frame",
        "pub fn decode_bloom_segment_frame",
    ] {
        assert!(
            file_bloom_segment.contains(required),
            "reddb-file should own bloom-segment persisted frame {required}"
        );
    }

    for forbidden in [
        "pub enum BloomSegmentError",
        "const HEADER_LEN",
        "bit_size.to_be_bytes()",
        "inserted.to_be_bytes()",
        "u32::from_be_bytes([bytes[2]",
        "u32::from_be_bytes([bytes[6]",
    ] {
        assert!(
            !bloom_segment.contains(forbidden),
            "bloom-segment persisted frame belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "pub const COLUMN_BLOCK_HEADER_LEN",
        "pub const COLUMN_BLOCK_DIR_ENTRY_LEN",
        "pub const COLUMN_BLOCK_FOOTER_LEN",
        "pub enum ColumnBlockFrameError",
        "pub struct ColumnBlockPart",
        "pub struct ColumnBlockFrame",
        "pub struct ColumnBlockColumn",
        "pub fn encode_column_block_frame",
        "pub fn decode_column_block_frame",
        "pub fn peek_column_block_version",
        "pub fn encode_column_block_granule_index_blob",
        "pub fn decode_column_block_granule_index_blob",
        "pub fn encode_column_block_granule_bloom_blob",
        "pub fn decode_column_block_granule_bloom_blob",
    ] {
        assert!(
            file_column_block.contains(required),
            "reddb-file should own column-block persisted frame {required}"
        );
    }
    for forbidden in [
        "out.extend_from_slice(&COLUMN_BLOCK_MAGIC)",
        "COLUMN_BLOCK_VERSION_V1.to_le_bytes()",
        "chunk_id.to_le_bytes()",
        "schema_ref.to_le_bytes()",
        "row_count.to_le_bytes()",
        "min_ts_ns.to_le_bytes()",
        "max_ts_ns.to_le_bytes()",
        "column_count as u32).to_le_bytes()",
        "stream.len() as u64).to_le_bytes()",
        "granule_cursor.to_le_bytes()",
        "bloom_cursor.to_le_bytes()",
        "u64::from_le_bytes(bytes[",
        "u32::from_le_bytes(bytes[",
        "let stored_crc",
        "let actual_crc",
    ] {
        assert!(
            !column_block_non_test.contains(forbidden),
            "column-block persisted envelope belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "pub enum ValueFlag",
        "pub enum ValueCodecError",
        "pub fn encode",
        "pub fn decode",
        "pub fn would_encode_to",
        "lz4_flex::compress",
        "lz4_flex::decompress",
    ] {
        assert!(
            file_vector_value_codec.contains(required),
            "reddb-file should own vector value codec contract {required}"
        );
    }
    for required in [
        "pub const GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN",
        "pub struct GraphLabelRegistryEntry",
        "pub enum GraphLabelRegistryFrameError",
        "pub fn encode_graph_label_registry_frame",
        "pub fn decode_graph_label_registry_frame",
    ] {
        assert!(
            file_graph_label_registry.contains(required),
            "reddb-file should own graph label-registry persisted frame {required}"
        );
    }
    for forbidden in [
        "entries.len() as u32).to_le_bytes()",
        "id.0.to_le_bytes()",
        "bytes.len() as u16).to_le_bytes()",
        "u32::from_le_bytes([data[0]",
        "u32::from_le_bytes([data[off]",
        "u16::from_le_bytes([data[off + 5]",
        "std::str::from_utf8(&data[off",
    ] {
        assert!(
            !graph_label_registry_non_test.contains(forbidden),
            "graph label-registry persisted frame belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "pub const GRAPH_NODE_HEADER_SIZE",
        "pub const GRAPH_NODE_HEADER_SIZE_V1",
        "pub const GRAPH_EDGE_HEADER_SIZE",
        "pub const GRAPH_EDGE_HEADER_SIZE_V1",
        "pub const GRAPH_TABLE_REF_SIZE",
        "pub struct GraphTableRef",
        "pub struct GraphNodeRecord",
        "pub struct LegacyGraphNodeRecord",
        "pub struct GraphEdgeRecord",
        "pub struct LegacyGraphEdgeRecord",
        "pub fn encode_graph_table_ref",
        "pub fn decode_graph_table_ref",
        "pub fn encode_graph_node_record_v2",
        "pub fn decode_graph_node_record_v2",
        "pub fn decode_graph_node_record_v1",
        "pub fn encode_graph_edge_record_v2",
        "pub fn decode_graph_edge_record_v2",
        "pub fn decode_graph_edge_record_v1",
    ] {
        assert!(
            file_graph_record.contains(required),
            "reddb-file should own graph node/edge persisted record {required}"
        );
    }
    for required in [
        "pub const GRAPH_STORE_MAGIC",
        "pub const GRAPH_STORE_VERSION_V1",
        "pub const GRAPH_STORE_VERSION_V2",
        "pub const GRAPH_STORE_HEADER_LEN",
        "pub struct GraphStoreFrame",
        "pub enum GraphStoreFrameError",
        "pub fn encode_graph_store_frame",
        "pub fn decode_graph_store_frame",
    ] {
        assert!(
            file_graph_store.contains(required),
            "reddb-file should own graph store persisted envelope {required}"
        );
    }
    for required in [
        "pub const GRAPH_TABLE_INDEX_HEADER_LEN",
        "pub const GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN",
        "pub const GRAPH_TABLE_INDEX_MAX_NODE_ID_LEN",
        "pub struct GraphTableIndexEntry",
        "pub enum GraphTableIndexFrameError",
        "pub fn encode_graph_table_index_frame",
        "pub fn decode_graph_table_index_frame",
    ] {
        assert!(
            file_graph_table_index.contains(required),
            "reddb-file should own graph table-index persisted frame {required}"
        );
    }
    for forbidden in [
        "buf[0..2].copy_from_slice(&self.table_id.to_le_bytes())",
        "buf[2..10].copy_from_slice(&self.row_id.to_le_bytes())",
        "id_bytes.len() as u16).to_le_bytes()",
        "label_bytes.len() as u16).to_le_bytes()",
        "self.label_id.as_u32().to_le_bytes()",
        "self.weight.to_le_bytes()",
        "source_bytes.len() as u16).to_le_bytes()",
        "target_bytes.len() as u16).to_le_bytes()",
        "u16::from_le_bytes([data[0]",
        "u32::from_le_bytes([data[4]",
        "f32::from_le_bytes([data[8]",
        "String::from_utf8_lossy(&data[EDGE_HEADER_SIZE",
        "String::from_utf8_lossy(&data[NODE_HEADER_SIZE",
    ] {
        assert!(
            !graph_store_non_test.contains(forbidden),
            "graph node/edge persisted record belongs in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in [
        "buf.extend_from_slice(b\"RBGR\")",
        "buf.extend_from_slice(&2u32.to_le_bytes())",
        "node_count.load(Ordering::Relaxed).to_le_bytes()",
        "edge_count.load(Ordering::Relaxed).to_le_bytes()",
        "registry_bytes.len() as u32).to_le_bytes()",
        "pages.len() as u32).to_le_bytes()",
        "&data[0..4] != b\"RBGR\"",
        "u32::from_le_bytes([data[4]",
        "u64::from_le_bytes([",
        "u32::from_le_bytes([",
        "Truncated registry blob",
        "Truncated node pages",
        "Truncated edge pages",
    ] {
        assert!(
            !graph_store_impl_non_test.contains(forbidden),
            "graph store persisted envelope belongs in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in [
        "mappings.len() as u32).to_le_bytes()",
        "id_bytes.len() as u16).to_le_bytes()",
        "u32::from_le_bytes([data[0]",
        "u16::from_le_bytes([data[offset]",
        "String::from_utf8_lossy(&data[offset",
        "TableRef::decode(&data[offset",
    ] {
        assert!(
            !graph_table_index_non_test.contains(forbidden),
            "graph table-index persisted frame belongs in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in [
        "pub enum ValueFlag",
        "pub enum ValueCodecError",
        "lz4_flex::",
        "input.len() as u32",
        "ValueCodecError::TruncatedHeader",
    ] {
        assert!(
            !vector_value_codec.contains(forbidden),
            "vector value persisted codec belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "pub enum PageType",
        "pub struct LeafCellFlags",
        "pub struct PageHeader",
        "pub struct LeafCell",
        "pub enum PageFormatError",
        "pub fn encode_leaf_cell_v2",
        "pub fn encode_leaf_cell_v1",
        "pub fn decode_leaf_cell",
    ] {
        assert!(
            file_vector_btree.contains(required),
            "reddb-file should own vector B-tree page format contract {required}"
        );
    }
    for forbidden in [
        "pub enum PageType",
        "pub struct LeafCellFlags",
        "pub struct PageHeader",
        "pub struct LeafCell",
        "pub enum PageFormatError",
        "FLAG_POINTER",
        "u16::from_le_bytes",
        "payload.len() as u32",
    ] {
        assert!(
            !vector_btree.contains(forbidden),
            "vector B-tree page format belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "pub enum BTreeValueCell",
        "pub enum BTreeValueCellError",
        "pub const BTREE_VALUE_OVERFLOW_THRESHOLD",
        "pub const BTREE_VALUE_MAX_SIZE",
        "pub const BTREE_VALUE_POINTER_CELL_LEN",
        "pub fn encode_btree_inline_raw",
        "pub fn encode_btree_inline_compressed",
        "pub fn encode_btree_pointer",
        "pub fn decode_btree_value_cell",
        "pub fn btree_value_pointer_head",
        "pub fn btree_projected_cell_len",
    ] {
        assert!(
            file_btree_value_layout.contains(required),
            "reddb-file should own B-tree value-cell persisted layout {required}"
        );
    }
    for forbidden in [
        "const FLAG_POINTER",
        "const FLAG_COMPRESSED",
        "const FLAG_RESERVED_MASK",
        "const POINTER_PAYLOAD_LEN",
        "head.to_le_bytes()",
        "total_len.to_le_bytes()",
        "u32::from_le_bytes",
        "u64::from_le_bytes",
        "0b0000_0001",
        "0b0000_0010",
        "0b0000_0100",
    ] {
        assert!(
            !btree_value_layout.contains(forbidden),
            "B-tree value-cell persisted layout belongs in reddb-file, found {forbidden:?}"
        );
    }

    assert!(
        page.contains("PAGE_FILE_MAGIC as MAGIC_BYTES")
            && page.contains("PAGE_FILE_VERSION as DB_VERSION")
            && page.contains("PAGED_PAGE_SIZE as PAGE_SIZE")
            && page.contains("PAGED_PAGE_HEADER_SIZE as HEADER_SIZE")
            && page.contains("reddb_file::encode_paged_page_header")
            && page.contains("reddb_file::decode_paged_page_header")
            && page_impl.contains("reddb_file::set_paged_page_lsn")
            && page_impl.contains("reddb_file::paged_page_checksum")
            && page_impl.contains("reddb_file::paged_cell_pointer")
            && page_impl.contains("reddb_file::write_paged_cell")
            && page_impl.contains("reddb_file::init_database_header_page")
            && page_impl.contains("reddb_file::database_header_page_count")
            && pager.contains("pub use reddb_file::{DatabaseHeader, PhysicalFileHeader}")
            && pager.contains("reddb_file::encode_paged_dwb_frame")
            && pager_impl.contains("reddb_file::encode_paged_dwb_frame")
            && pager_impl.contains("reddb_file::decode_paged_dwb_frame")
            && pager_impl.contains("reddb_file::decode_database_header")
            && pager_impl.contains("reddb_file::encode_database_header")
            && pager_impl.contains("reddb_file::paged_encryption_marker_present")
            && pager_impl.contains("reddb_file::write_paged_encryption_marker_and_header")
            && column_block
                .contains("pub use reddb_file::{COLUMN_BLOCK_MAGIC, COLUMN_BLOCK_VERSION_V1}"),
        "server should compatibility-reexport/use core file constants from reddb-file"
    );
    assert!(
        bloom_segment.contains("pub use reddb_file::BloomSegmentFrameError as BloomSegmentError;")
            && bloom_segment.contains("reddb_file::encode_bloom_segment_frame")
            && bloom_segment.contains("reddb_file::decode_bloom_segment_frame")
            && vector_value_codec.contains("pub use reddb_file::vector_value_codec::{")
            && vector_btree.contains("pub use reddb_file::vector_btree_page_format::{")
            && btree_value_layout.contains("reddb_file::decode_btree_value_cell")
            && btree_value_layout.contains("reddb_file::encode_btree_pointer")
            && btree_value_layout.contains("reddb_file::btree_projected_cell_len")
            && column_block.contains("reddb_file::encode_column_block_frame")
            && column_block.contains("reddb_file::decode_column_block_frame")
            && column_block.contains("reddb_file::encode_column_block_granule_index_blob")
            && column_block.contains("reddb_file::decode_column_block_granule_bloom_blob")
            && graph_label_registry.contains("reddb_file::encode_graph_label_registry_frame")
            && graph_label_registry.contains("reddb_file::decode_graph_label_registry_frame")
            && graph_store.contains("reddb_file::encode_graph_node_record_v2")
            && graph_store.contains("reddb_file::decode_graph_node_record_v2")
            && graph_store.contains("reddb_file::encode_graph_edge_record_v2")
            && graph_store.contains("reddb_file::decode_graph_edge_record_v2")
            && graph_store_impl.contains("reddb_file::encode_graph_store_frame")
            && graph_store_impl.contains("reddb_file::decode_graph_store_frame")
            && graph_table_index.contains("reddb_file::encode_graph_table_index_frame")
            && graph_table_index.contains("reddb_file::decode_graph_table_index_frame")
            && physical.contains("PHYSICAL_METADATA_PROTOCOL_VERSION"),
        "server should import bloom/vector/physical file contracts from reddb-file"
    );
}

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
fn server_uses_reddb_file_for_logical_wal_spool_paths() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/primary.rs"));

    for forbidden in [
        "format!(\"{file_name}.logical.wal\")",
        "with_extension(\"logical.wal.tmp\")",
    ] {
        assert!(
            !text.contains(forbidden),
            "logical WAL spool paths are a reddb_file::layout contract"
        );
    }
    assert!(
        text.contains("reddb_file::layout::logical_wal_path"),
        "logical WAL spool path should route through reddb-file"
    );
    assert!(
        text.contains("reddb_file::layout::logical_wal_temp_path"),
        "logical WAL prune temp path should route through reddb-file"
    );
}

#[test]
fn server_does_not_redeclare_logical_wal_spool_format() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/primary.rs"));

    for forbidden in [
        "const LOGICAL_WAL_SPOOL_MAGIC",
        "const LOGICAL_WAL_SPOOL_VERSION",
        "const LOGICAL_WAL_V3_HEADER_LEN",
        "compute_logical_v2_crc",
        "compute_logical_v3_crc",
        "fn read_and_repair_entries",
        "fn read_entries_from",
        "fn build_seek_index",
        "fn read_one_v3",
        "fn read_one_v2",
        "fn read_one_v1",
    ] {
        assert!(
            !text.contains(forbidden),
            "logical WAL spool binary format belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::encode_logical_wal_v3",
        "reddb_file::read_and_repair_logical_wal_entries",
        "reddb_file::read_logical_wal_entries_from",
        "reddb_file::build_logical_wal_seek_index",
        "reddb_file::rewrite_logical_wal_entries",
    ] {
        assert!(
            text.contains(required),
            "logical WAL spool should route through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_relay_segment_names() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/runtime/impl_primary_replica_file.rs"));

    assert!(
        !text.contains("relay-{start_lsn:020}-{end_lsn:020}.redwal"),
        "relay segment names are a reddb_file::layout contract"
    );
    assert!(
        text.contains("reddb_file::layout::relay_segment_relative_path"),
        "runtime should route relay segment names through reddb-file"
    );
}

#[test]
fn server_uses_reddb_file_for_serverless_roots_and_cache() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/runtime/impl_serverless.rs"));

    for forbidden in [
        "with_extension(\"serverless\")",
        "file_stem()",
        ".join(\"cache\")",
    ] {
        assert!(
            !text.contains(forbidden),
            "serverless root/cache filename contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::ServerlessFilePlan::for_data_path",
        ".for_generation(generation)",
        ".local_cache()",
    ] {
        assert!(
            text.contains(required),
            "serverless runtime should route through {required}"
        );
    }
}

#[test]
fn server_does_not_own_serverless_writer_lease_artifact() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/lease.rs"));

    for forbidden in [
        "pub struct WriterLease",
        "fn to_json",
        "fn from_json",
        "JsonValue",
        "serde_json::",
        "\"database_key\"",
        "\"holder_id\"",
        "\"generation\"",
        "\"acquired_at_ms\"",
        "\"expires_at_ms\"",
        "{}{}.lease.json",
        "reddb-lease-{kind}",
    ] {
        assert!(
            !text.contains(forbidden),
            "serverless writer lease artifact contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub use reddb_file::ServerlessWriterLease as WriterLease",
        "reddb_file::serverless_writer_lease_key",
        "reddb_file::serverless_writer_lease_temp_path",
        "reddb_file::encode_serverless_writer_lease_json",
        "reddb_file::decode_serverless_writer_lease_json",
    ] {
        assert!(
            text.contains(required),
            "serverless writer lease runtime should route through {required}"
        );
    }
}

#[test]
fn server_does_not_redeclare_election_or_fence_file_stores() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/replication/election.rs",
        "crates/reddb-server/src/replication/fence.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "pub struct FileLastVoteStore",
            "pub struct FileTermStore",
            "with_extension(\"lastvote.tmp\")",
            "with_extension(\"term.tmp\")",
            "serde_json::from_slice",
            "serde_json::to_vec",
        ] {
            assert!(
                !text.contains(forbidden),
                "election/fence file-store contracts belong in reddb-file, found {forbidden:?}"
            );
        }
    }

    let election = read(root.join("crates/reddb-server/src/replication/election.rs"));
    assert!(election.contains("pub use reddb_file::FileLastVoteStore"));
    assert!(election.contains("reddb_file::DurableLastVote"));

    let fence = read(root.join("crates/reddb-server/src/replication/fence.rs"));
    assert!(fence.contains("pub use reddb_file::FileTermStore"));
}

#[test]
fn server_does_not_redeclare_zone_map_file_format() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/index/zone_map_persist.rs"));

    for forbidden in [
        "const MAGIC",
        "const VERSION",
        "pub struct PersistedZone",
        "pub enum ZoneMapPersistError",
        "with_extension(\"zonemap.tmp\")",
        "fn read_u32",
        "fn read_u64",
        "fn read_str",
        "fn write_str",
    ] {
        assert!(
            !text.contains(forbidden),
            "zone-map sidecar format belongs in reddb-file, found {forbidden:?}"
        );
    }

    assert!(
        text.contains("reddb_file::{"),
        "server zone_map_persist module should only reexport reddb-file contracts"
    );
}

#[test]
fn server_does_not_redeclare_turboquant_snapshot_format() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/engine/turboquant/snapshot.rs"));

    for forbidden in [
        ".TVSNAP",
        "const MAGIC",
        "const VERSION",
        "pub const HEADER_BYTES",
        "pub struct SnapshotPayload",
        "pub enum SnapshotError",
        "with_extension(\"tv.tmp\")",
        "fn crc32",
    ] {
        assert!(
            !text.contains(forbidden),
            "turboquant snapshot format belongs in reddb-file, found {forbidden:?}"
        );
    }

    assert!(
        text.contains("reddb_file::{"),
        "server turboquant snapshot module should only reexport reddb-file contracts"
    );
}

#[test]
fn server_does_not_redeclare_blob_cache_l2_file_format() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/storage/cache/mod.rs",
        "crates/reddb-server/src/storage/cache/blob/entry.rs",
        "crates/reddb-server/src/storage/cache/blob/l2.rs",
        "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "const L2_CONTROL_MAGIC",
            "const L2_METADATA_MAGIC",
            "const L2_BLOB_MAGIC",
            "const L2_FORMAT_V1_RAW",
            "const L2_FORMAT_V2_FRAMED",
            "const L2_FRAME_TAG_RAW",
            "const L2_FRAME_TAG_ZSTD",
            "pub(super) struct L2Control",
            "pub(super) struct L2Record",
            "fn encode_v2_frame",
            "fn decode_v2_frame",
            "with_extension(\"blob-cache.ctl\")",
            "with_extension(\"ctl.tmp\")",
            "blob-cache.ctl",
            "const L2_BACKUP_PAGER_SUFFIX",
            "const L2_BACKUP_CONTROL_SUFFIX",
            "l2.pager",
            "l2.ctl",
        ] {
            assert!(
                !text.contains(forbidden),
                "blob-cache L2 file format belongs in reddb-file, found {forbidden:?} in {file}"
            );
        }
    }

    let l2 = read(root.join("crates/reddb-server/src/storage/cache/blob/l2.rs"));
    for required in [
        "reddb_file::{",
        "blob_cache_control_path",
        "encode_l2_key",
        "encode_l2_v2_frame",
        "decode_l2_v2_frame",
        "L2BlobFrame",
        "L2Control",
        "L2Record",
        "L2_BLOB_MAGIC",
        "L2_FORMAT_V1_RAW",
        "L2_FORMAT_V2_FRAMED",
    ] {
        assert!(
            l2.contains(required),
            "blob-cache L2 runtime should route persisted file contracts through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_replica_rebootstrap_paths() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/replica.rs"));

    for forbidden in [
        "with_extension(\"rebootstrap.redbase\")",
        "with_extension(\"rebootstrap.pending.rdb\")",
        "with_extension(\"rebootstrap.ready\")",
        "with_extension(\"rebootstrap.intent.jsonl\")",
        "with_extension(\"rebootstrap.previous.rdb\")",
        "with_extension(\"tmp\")",
        "\"pending_path\": ready.pending_path",
        "\"checkpoint_lsn\": ready.checkpoint_lsn",
        "\"timeline\": ready.timeline.0",
        "from_slice(&std::fs::read(&marker_path)",
        "InvalidField(\"pending_path\")",
    ] {
        assert!(
            !text.contains(forbidden),
            "replica rebootstrap artifact names belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::layout::rebootstrap_staging_root",
        "reddb_file::layout::rebootstrap_pending_path",
        "reddb_file::layout::rebootstrap_ready_marker_path",
        "reddb_file::layout::rebootstrap_intent_log_path",
        "reddb_file::layout::rebootstrap_previous_path",
        "reddb_file::layout::atomic_temp_path",
        "reddb_file::write_rebootstrap_ready_marker",
        "reddb_file::read_rebootstrap_ready_marker",
    ] {
        assert!(
            text.contains(required),
            "replica rebootstrap pathing should route through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_primary_replica_slot_paths() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/replication/primary.rs",
        "crates/reddb-server/src/replication/primary/slots.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "with_extension(\"primary-replica\")",
            "logical.slots.json",
            "with_extension(\"logical.slots.tmp\")",
        ] {
            assert!(
                !text.contains(forbidden),
                "primary-replica slot artifact names belong in reddb-file, found {forbidden:?}"
            );
        }
    }

    let primary = read(root.join("crates/reddb-server/src/replication/primary.rs"));
    assert!(primary.contains("reddb_file::layout::legacy_logical_slots_path"));
    assert!(primary.contains("reddb_file::layout::primary_replica_root"));

    let slots = read(root.join("crates/reddb-server/src/replication/primary/slots.rs"));
    assert!(slots.contains("read_legacy_json_from_path"));
    assert!(slots.contains("write_legacy_json_to_path"));
}

#[test]
fn server_does_not_redeclare_replication_slot_file_contracts() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/replication/primary.rs",
        "crates/reddb-server/src/replication/primary/slots.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "struct ReplicationSlot",
            "pub struct ReplicationSlot",
            "enum SlotInvalidationCause",
            "pub enum SlotInvalidationCause",
            "legacy_logical_slots_temp_path",
            "serde_json::",
            "\"restart_lsn\"",
            "\"confirmed_lsn\"",
            "\"last_seen_at_unix_ms\"",
            "\"invalidation_reason\"",
            "\"invalidated_at_unix_ms\"",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} must use reddb_file for replication slot file contracts"
            );
        }
    }
}

#[test]
fn server_operational_manifest_is_runtime_alias_only() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/operational_manifest.rs"));

    for forbidden in [
        "struct Manifest",
        "struct OperationalManifest",
        "manifest_to_bytes",
        "manifest_from_bytes",
        "checksum_manifest",
        "const MANIFEST_FILE",
    ] {
        assert!(
            !text.contains(forbidden),
            "operational manifest persistence belongs in reddb-file, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("reddb_file::OperationalManifest"),
        "server should route operational manifest calls through reddb-file"
    );
}

#[test]
fn server_does_not_own_backup_or_wal_archive_manifest_codecs() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/archiver.rs"));

    for forbidden in [
        "pub struct BackupHead",
        "pub struct SnapshotManifest",
        "pub struct WalSegmentManifest",
        "pub struct UnifiedManifest",
        "pub struct UnifiedSnapshotEntry",
        "pub struct UnifiedWalEntry",
        "fn to_json_value",
        "fn from_json_value",
        "write_json_object",
        "read_json_object",
        "snapshot manifest missing",
        "wal segment manifest missing",
        "unified manifest must be a JSON object",
        "JsonValue::Array(",
        "object.insert(\"lsn\"",
        "object.insert(\"data\"",
        "hex::encode",
        "hex::decode",
        "archived logical wal must be a JSON array",
        "decode wal record hex failed",
        "{:012}-{:012}.wal",
    ] {
        assert!(
            !text.contains(forbidden),
            "backup/WAL archive manifest codecs belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::encode_unified_manifest_json",
        "reddb_file::decode_unified_manifest_json",
        "reddb_file::encode_wal_segment_manifest_json",
        "reddb_file::decode_wal_segment_manifest_json",
        "reddb_file::encode_backup_head_json",
        "reddb_file::decode_backup_head_json",
        "reddb_file::encode_snapshot_manifest_json",
        "reddb_file::decode_snapshot_manifest_json",
        "reddb_file::encode_archived_logical_wal_records",
        "reddb_file::decode_archived_logical_wal_records",
        "reddb_file::archived_wal_segment_key",
    ] {
        assert!(
            text.contains(required),
            "backup/WAL archive manifest runtime should route through {required}"
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
        "reddb_file::layout::physical_metadata_journal_prefix",
        "reddb_file::layout::physical_export_data_path",
        "reddb_file::read_physical_metadata_document",
        "reddb_file::write_physical_metadata_json_document",
        "reddb_file::write_physical_metadata_binary_document",
    ] {
        assert!(
            text.contains(required),
            "physical metadata pathing should route through {required}"
        );
    }
}

#[test]
fn server_does_not_own_physical_metadata_document_codec() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/physical/metadata_file.rs"));

    for forbidden in [
        "fs::read_to_string(path)",
        "fs::read(path)",
        "fs::write(path, text)",
        "fs::write(path, bytes)",
        "from_slice::<JsonValue>",
        "to_vec(&self.to_json_value())",
    ] {
        assert!(
            !text.contains(forbidden),
            "physical metadata document codec belongs in reddb-file, found {forbidden:?}"
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
}

#[test]
fn server_does_not_own_spill_file_format() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/storage/cache/spill.rs"));
    let file = read(root.join("crates/reddb-file/src/spill.rs"));

    for forbidden in [
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
            !server.contains(forbidden),
            "spill file frame belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
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

    assert!(server.contains("reddb_file::encode_spill_file_frame"));
    assert!(server.contains("reddb_file::decode_spill_file_frame"));
    assert!(server.contains("reddb_file::spill_file_name"));
}
