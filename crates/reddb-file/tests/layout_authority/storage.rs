use crate::common::*;

#[test]
fn reddb_file_source_modules_stay_under_two_thousand_lines() {
    let root = repo_root();
    for path in rust_files_under(&root.join("crates/reddb-file/src")) {
        let text = read(&path);
        let line_count = text.lines().count();
        assert!(
            line_count <= 2_000,
            "{} has {line_count} lines; split file-contract modules before adding more authority",
            path.display()
        );
    }
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
        "crates/reddb-server/src/storage/wal/append_coordinator.rs",
        "crates/reddb-server/src/storage/wal/reader.rs",
        "crates/reddb-server/src/storage/wal/writer.rs",
        "crates/reddb-server/src/storage/wal/group_commit.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "wal_path_for_db",
            "rb_commit_coord_",
            "rb_wal_coord_",
            "rb_wal_reader_",
            "rb_wal_writer_",
            "rb_group_commit_",
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

    let reader = read(root.join("crates/reddb-server/src/storage/wal/reader.rs"));
    let writer = read(root.join("crates/reddb-server/src/storage/wal/writer.rs"));
    let coordinator = read(root.join("crates/reddb-server/src/storage/wal/append_coordinator.rs"));
    assert!(
        reader.contains("reddb_file::layout::wal_component_temp_path")
            && writer.contains("reddb_file::layout::wal_component_temp_path")
            && coordinator.contains("reddb_file::layout::wal_component_unique_temp_path"),
        "WAL component temp paths should route through reddb-file layout"
    );
}

#[test]
fn server_uses_reddb_file_for_pager_shadow_sidecar_groups() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/storage/engine/btree.rs",
        "crates/reddb-server/src/storage/engine/overflow.rs",
        "crates/reddb-server/src/storage/engine/btree/value_layout.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        assert!(
            !text.contains("[\"-hdr\", \"-meta\", \"-dwb\"]"),
            "{file} should not rebuild pager shadow sidecar suffix groups"
        );
        assert!(
            text.contains("reddb_file::layout::pager_shadow_sidecar_paths"),
            "{file} should route pager shadow sidecar groups through reddb-file"
        );
    }

    let layout = read(root.join("crates/reddb-file/src/layout.rs"));
    assert!(
        layout.contains("pub fn pager_shadow_sidecar_paths"),
        "reddb-file should own the pager shadow sidecar group"
    );
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
fn server_does_not_own_native_store_atomic_publish() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/unified/store/impl_file.rs"));

    for forbidden in [
        "reddb_file::temp_path(path)",
        "File::create(&tmp_path)",
        "BufWriter::new(file)",
        "writer.get_ref().sync_all()",
        "std::fs::rename(&tmp_path, path)",
        "File::open(parent)",
        "dir.sync_all()",
    ] {
        assert!(
            !text.contains(forbidden),
            "native store atomic publish belongs in reddb-file, found {forbidden:?}"
        );
    }

    assert!(
        text.contains("reddb_file::write_native_store_bytes_atomically(path, &buf)"),
        "UnifiedStore::save_to_file should publish native store bytes through reddb-file"
    );
}

#[test]
fn server_does_not_own_main_wal_file_header() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/storage/wal/mod.rs",
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
            "RDBW",
            "Version (1 byte)",
            "Reserved (3 bytes)",
            "CRC32 checksum",
            "extend_from_slice(WAL_MAGIC)",
            "push(WAL_VERSION)",
            "Invalid WAL magic bytes",
            "const WAL_SEGMENT_BYTES",
            "fn next_wal_segment_boundary",
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
    assert!(
        writer.contains("reddb_file::next_main_wal_segment_boundary"),
        "WAL writer should plan segment boundaries through reddb-file"
    );
    assert!(
        writer.contains("reddb_file::MAIN_WAL_SEGMENT_BYTES"),
        "WAL writer should use main WAL segment sizing from reddb-file"
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
fn server_wal_record_tests_do_not_assert_physical_tags() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/record.rs"));
    let test_source = text.split("#[cfg(test)]").nth(1).unwrap_or("");

    for forbidden in [
        "RecordType::from_u8(",
        "encoded[0]",
        "Type (1)",
        "WAL_FILE_VERSION_V2",
        "WAL_FILE_MAGIC",
        "assert_eq!(WAL_FILE_VERSION",
        "crc32fast::Hasher",
    ] {
        assert!(
            !test_source.contains(forbidden),
            "server WAL tests should not assert persisted WAL byte contracts, found {forbidden:?}"
        );
    }

    let file = read(root.join("crates/reddb-file/src/wal_record.rs"));
    for required in [
        "main_wal_record_types_are_stable",
        "main_wal_record_accepts_legacy_v2_without_term",
        "main_wal_record_compresses_and_decompresses_page_writes",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own WAL byte-contract test {required}"
        );
    }
}

#[test]
fn server_checkpoint_tests_do_not_assert_wal_physical_sizes() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/checkpoint.rs"));
    let test_source = text.split("#[cfg(test)]").nth(1).unwrap_or("");

    for forbidden in [
        "Header (8 bytes)",
        "Checkpoint record (1 + 8 + 4",
        "WAL should be truncated, but size",
        "fs::metadata(&wal_path).unwrap().len()",
        "dir.join(\"test.wal\")",
    ] {
        assert!(
            !test_source.contains(forbidden),
            "server checkpoint tests should assert WAL semantics, not byte sizes: {forbidden:?}"
        );
    }

    assert!(
        test_source.contains("reddb_file::layout::wal_component_temp_path"),
        "server checkpoint tests should derive WAL fixture names through reddb-file"
    );

    assert!(
        test_source.contains("WalRecord::Checkpoint")
            && test_source.contains("lsn: result.checkpoint_lsn"),
        "server checkpoint truncate test should validate the semantic checkpoint marker"
    );
}

#[test]
fn server_wal_tests_use_reddb_file_temp_wal_names() {
    let root = repo_root();
    for file in [
        "crates/reddb-server/src/storage/wal/mod.rs",
        "crates/reddb-server/src/storage/wal/archiver.rs",
        "crates/reddb-server/src/storage/wal/checkpoint.rs",
    ] {
        let text = read(root.join(file));
        let test_source = text.split("#[cfg(test)]").nth(1).unwrap_or("");
        for forbidden in [
            "dir.join(\"test.wal\")",
            "temp_dir.join(\"test.wal\")",
            "temp_dir.join(\"downloaded.wal\")",
        ] {
            assert!(
                !test_source.contains(forbidden),
                "{file} WAL fixtures should use reddb-file layout helpers, found {forbidden:?}"
            );
        }
        assert!(
            test_source.contains("reddb_file::layout::wal_component_temp_path"),
            "{file} should derive WAL fixture paths through reddb-file"
        );
    }
}

#[test]
fn server_does_not_own_transaction_wal_record_envelope() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/transaction/log.rs"));
    let non_test = text
        .split("#[cfg(test)]")
        .next()
        .expect("transaction/log.rs has non-test source");
    let file = read(root.join("crates/reddb-file/src/transaction_wal.rs"));
    let layout = read(root.join("crates/reddb-file/src/layout.rs"));

    for forbidden in [
        "PathBuf::from(\"wal.log\")",
        "buf.extend(&self.lsn.to_le_bytes())",
        "buf.extend(&self.txn_id.to_le_bytes())",
        "buf.extend(&self.prev_lsn.unwrap_or(0).to_le_bytes())",
        "buf.extend(&self.timestamp.to_le_bytes())",
        "buf.extend(&(type_bytes.len() as u32).to_le_bytes())",
        "buf.push(0)",
        "buf.push(1)",
        "buf.push(2)",
        "buf.push(3)",
        "buf.push(4)",
        "buf.push(5)",
        "buf.push(6)",
        "buf.push(7)",
        "buf.push(8)",
        "buf.push(9)",
        "buf.push(10)",
        "read_u32(data, &mut offset",
        "read_u64(data, &mut offset",
        "let computed: u8 = data[..offset].iter().fold(0",
        "Checksum mismatch",
        "Missing WAL entry checksum",
        "Missing WAL entry LSN",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "transaction WAL record envelope belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub const TRANSACTION_WAL_FILE_NAME",
        "pub fn default_transaction_wal_path",
    ] {
        assert!(
            layout.contains(required),
            "reddb-file layout should own transaction WAL path contract {required}"
        );
    }

    for required in [
        "pub struct TransactionWalRecordFrame",
        "pub fn encode_transaction_wal_record_frame",
        "pub fn decode_transaction_wal_record_frame",
        "pub fn transaction_wal_record_encoded_len",
        "pub enum TransactionWalEntryPayload",
        "pub fn encode_transaction_wal_entry_payload",
        "pub fn decode_transaction_wal_entry_payload",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own transaction WAL contract {required}"
        );
    }

    for required in [
        "reddb_file::TransactionWalRecordFrame",
        "reddb_file::layout::default_transaction_wal_path",
        "reddb_file::encode_transaction_wal_record_frame",
        "reddb_file::decode_transaction_wal_record_frame",
        "reddb_file::transaction_wal_record_encoded_len",
        "reddb_file::TransactionWalEntryPayload",
        "reddb_file::encode_transaction_wal_entry_payload",
        "reddb_file::decode_transaction_wal_entry_payload",
    ] {
        assert!(
            non_test.contains(required),
            "transaction WAL runtime should route persisted contract through {required}"
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
            "wal.log",
            "rdb-wal",
            "with_extension(\"rdb-wal\")",
            "with_extension(\"rdb-hdr\")",
            "with_extension(\"rdb-meta\")",
            "with_extension(\"rdb-dwb\")",
            "set_extension(\"wal\")",
            "with_extension(\"shm\")",
            "push(\"-hdr\")",
            "push(\"-meta\")",
            "push(\"-dwb\")",
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
fn tiered_layout_matrix_lives_in_reddb_file_tests() {
    let root = repo_root();
    let file_test = read(root.join("crates/reddb-file/tests/storage_layout.rs"));
    assert!(
        file_test.contains("use reddb_file::{")
            && file_test.contains("standard_layout_is_default_and_derives_stable_sidecar_paths"),
        "tiered layout contract tests should live in reddb-file"
    );

    let server_test = root.join("crates/reddb-server/tests/storage_layout.rs");
    assert!(
        !server_test.exists(),
        "server must not own the tiered layout matrix test"
    );
}

#[test]
fn embedded_rdb_artifact_contract_tests_live_in_reddb_file() {
    let root = repo_root();
    let file_test = read(root.join("crates/reddb-file/tests/embedded_rdb_artifact.rs"));
    let server_test = read(root.join("crates/reddb-server/tests/embedded_rdb_skeleton.rs"));

    for required in [
        "open_falls_back_to_older_superblock_when_newer_copy_is_invalid",
        "open_validates_manifest_checksum_from_selected_superblock",
        "embedded_wal_frames_are_versioned_ordered_and_chained",
        "embedded_snapshot_crash_injection_preserves_published_snapshot",
    ] {
        assert!(
            file_test.contains(required),
            "embedded .rdb artifact contract test should live in reddb-file: {required}"
        );
        assert!(
            !server_test.contains(required),
            "server embedded tests should stay runtime-level and not own {required}"
        );
    }
}

#[test]
fn server_runtime_uses_file_owned_default_database_path() {
    let root = repo_root();
    let runtime_files = [
        "crates/reddb-server/src/engine.rs",
        "crates/reddb-server/src/storage/unified/devx/reddb/impl_core_a.rs",
        "crates/reddb-server/src/runtime/impl_core.rs",
    ];

    for file in runtime_files {
        let text = read(root.join(file));
        assert!(
            !text.contains("resolved_path(\"data.rdb\")"),
            "{file} must ask reddb-file for the default database path"
        );
        assert!(
            text.contains("reddb_file::default_database_path()"),
            "{file} should route default database path through reddb-file"
        );
    }

    let cli_commands = read(root.join("crates/reddb-server/src/cli/commands.rs"));
    assert!(
        !cli_commands.contains(".with_default(\"/var/lib/reddb/data.rdb\")"),
        "service CLI database path default should route through reddb-file"
    );
    assert!(
        cli_commands.contains("reddb_file::DEFAULT_SERVICE_DATABASE_PATH"),
        "service CLI database path default should use reddb-file's service default"
    );
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
fn server_uses_reddb_file_for_audit_log_paths() {
    let root = repo_root();
    let audit_log = read(root.join("crates/reddb-server/src/runtime/audit_log.rs"));
    let audit_query = read(root.join("crates/reddb-server/src/runtime/audit_query.rs"));
    let non_test_log = audit_log
        .split("#[cfg(test)]")
        .next()
        .expect("audit_log.rs has non-test source");
    let non_test_query = audit_query
        .split("#[cfg(test)]")
        .next()
        .expect("audit_query.rs has non-test source");

    for forbidden in [
        "parent.join(\".audit.log\")",
        "unwrap_or(\".audit.log\")",
        "format!(\"{stem}.{ts}\")",
        "format!(\"{stem}.{ts}.zst\")",
        "std::fs::rename(active",
        "zstd::bulk::compress",
        "std::fs::remove_file(&plain)",
    ] {
        assert!(
            !non_test_log.contains(forbidden),
            "audit log path contracts belong in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "reddb_file::layout::legacy_audit_log_path",
        "reddb_file::rotate_audit_log",
    ] {
        assert!(
            non_test_log.contains(required),
            "audit log path handling should route through {required}"
        );
    }

    let file_audit = read(root.join("crates/reddb-file/src/audit_log.rs"));
    for required in [
        "pub fn rotate_audit_log",
        "crate::layout::audit_log_rotated_plain_path",
        "crate::layout::audit_log_rotated_compressed_path",
        "zstd::bulk::compress",
    ] {
        assert!(
            file_audit.contains(required),
            "reddb-file should own audit log rotation lifecycle {required}"
        );
    }

    for forbidden in [
        "unwrap_or(\".audit.log\")",
        "trim_end_matches(\".zst\")",
        "e == \"zst\"",
    ] {
        assert!(
            !non_test_query.contains(forbidden),
            "audit query rotated-name contracts belong in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "reddb_file::layout::parse_audit_log_rotated_timestamp",
        "reddb_file::layout::AUDIT_LOG_ROTATED_COMPRESSED_EXTENSION",
    ] {
        assert!(
            non_test_query.contains(required),
            "audit query path handling should route through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_slow_query_log_paths() {
    let root = repo_root();
    let slow_logger = read(root.join("crates/reddb-server/src/telemetry/slow_query_logger.rs"));
    let non_test = slow_logger
        .split("#[cfg(test)]")
        .next()
        .expect("slow_query_logger.rs has non-test source");

    assert!(
        !non_test.contains("join(\"red-slow.log\")"),
        "slow-query fallback log path belongs in reddb-file"
    );
    assert!(
        non_test.contains("reddb_file::layout::legacy_slow_query_log_path"),
        "slow-query fallback log path should route through reddb-file"
    );
}

#[test]
fn server_does_not_redeclare_physical_metadata_core_contracts() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/physical.rs"));
    let file = read(root.join("crates/reddb-file/src/physical_metadata.rs"));
    let types = read(root.join("crates/reddb-file/src/physical_metadata/types.rs"));
    let policy = read(root.join("crates/reddb-file/src/physical_metadata_policy.rs"));

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
        "static META_JSON_SIDECAR_POLICY",
        "static SEQN_JOURNAL_POLICY",
        "static FOLD_PAGER_META_POLICY",
        "static FOLD_DWB_INTO_WAL_POLICY",
        "pub fn meta_json_sidecar_enabled",
        "pub fn seqn_journal_enabled",
        "pub fn fold_pager_meta_enabled",
        "pub fn fold_dwb_into_wal_enabled",
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
        "static META_JSON_SIDECAR_POLICY",
        "static SEQN_JOURNAL_POLICY",
        "static FOLD_PAGER_META_POLICY",
        "static FOLD_DWB_INTO_WAL_POLICY",
        "pub fn meta_json_sidecar_enabled",
        "pub fn seqn_journal_enabled",
        "pub fn fold_pager_meta_enabled",
        "pub fn fold_dwb_into_wal_enabled",
    ] {
        assert!(
            file.contains(required) || types.contains(required) || policy.contains(required),
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
    let server_unified_files = rust_files_under(
        &root
            .join("crates/reddb-server/src/storage/unified")
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
        "pub fn encode_native_store_header",
        "pub fn decode_native_store_header",
        "pub fn native_store_magic_matches",
        "pub fn encode_native_entity_record_frame",
        "pub fn decode_native_entity_record_frame",
        "pub fn encode_native_metadata_overflow_header",
        "pub fn decode_native_metadata_overflow_header",
        "pub fn encode_native_metadata_overflow_continuation_header",
        "pub fn decode_native_metadata_overflow_continuation_header",
        "pub fn encode_native_paged_metadata_header",
        "pub fn decode_native_paged_metadata_header",
        "pub fn encode_native_len_prefixed_bytes",
        "pub fn encode_native_len_prefixed_str",
        "pub fn decode_native_len_prefixed_bytes",
        "pub fn decode_native_len_prefixed_string",
        "pub fn encode_native_paged_collection_root",
        "pub fn decode_native_paged_collection_root",
        "pub fn encode_native_paged_cross_ref",
        "pub fn decode_native_paged_cross_ref",
        "pub fn encode_native_dump_count",
        "pub fn decode_native_dump_count",
        "pub fn encode_native_dump_collection_header",
        "pub fn decode_native_dump_collection_header",
        "pub fn encode_native_dump_entity_record",
        "pub fn decode_native_dump_entity_record",
        "pub fn encode_native_dump_cross_ref",
        "pub fn decode_native_dump_cross_ref",
        "pub fn append_native_store_crc32_footer",
        "pub fn verify_native_store_crc32_footer",
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
            "ENTITY_RECORD_MAGIC",
            "METADATA_OVERFLOW_MAGIC",
            "const ENTITY_RECORD_MAGIC",
            "const METADATA_OVERFLOW_MAGIC",
            "extend_from_slice(METADATA_MAGIC)",
            "&content[0..4] == METADATA_MAGIC",
            "Invalid magic bytes - expected RDST",
            "Unsupported version:",
            "Binary store CRC32 mismatch",
            "buf.extend_from_slice(STORE_MAGIC)",
            "crc32::crc32(&buf",
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

    for path in server_unified_files {
        let text = read(&path);
        assert!(
            !text.contains("b\"RDST\""),
            "{} must delegate native store magic matching to reddb-file",
            path.display()
        );
    }

    let impl_file = read(root.join("crates/reddb-server/src/storage/unified/store/impl_file.rs"));
    for required in [
        "reddb_file::decode_native_store_header",
        "reddb_file::verify_native_store_crc32_footer",
        "reddb_file::encode_native_store_header",
        "reddb_file::append_native_store_crc32_footer",
        "reddb_file::encode_native_dump_count",
        "reddb_file::decode_native_dump_count",
        "reddb_file::encode_native_dump_collection_header",
        "reddb_file::decode_native_dump_collection_header",
        "reddb_file::encode_native_dump_entity_record",
        "reddb_file::decode_native_dump_entity_record",
        "reddb_file::encode_native_dump_cross_ref",
        "reddb_file::decode_native_dump_cross_ref",
    ] {
        assert!(
            impl_file.contains(required),
            "UnifiedStore dump header/footer should route through {required}"
        );
    }

    for forbidden in [
        "Failed to read collection count",
        "Failed to read name length",
        "Failed to read entity count",
        "Truncated entity record length",
        "Truncated entity record payload",
        "Failed to read cross-ref count",
        "Failed to read source_id",
        "Failed to read target_id",
        "Failed to read collection length",
        "Invalid UTF-8 in collection:",
        "buf.extend_from_slice(&(record.len() as u32).to_le_bytes());",
        "write_varu32(&mut buf, collections.len() as u32);",
        "write_varu32(&mut buf, total_refs as u32);",
        "write_varu64(&mut buf, source_id.raw());",
        "write_varu64(&mut buf, target_id.raw());",
    ] {
        assert!(
            !impl_file.contains(forbidden),
            "UnifiedStore dump envelope should route through reddb-file, found {forbidden:?}"
        );
    }

    let impl_pages = read(root.join("crates/reddb-server/src/storage/unified/store/impl_pages.rs"));
    for required in [
        "reddb_file::encode_native_entity_record_frame",
        "reddb_file::decode_native_entity_record_frame",
        "reddb_file::encode_native_metadata_overflow_header",
        "reddb_file::decode_native_metadata_overflow_header",
        "reddb_file::encode_native_metadata_overflow_continuation_header",
        "reddb_file::decode_native_metadata_overflow_continuation_header",
        "reddb_file::encode_native_paged_metadata_header",
        "reddb_file::decode_native_paged_metadata_header",
        "reddb_file::encode_native_len_prefixed_bytes",
        "reddb_file::encode_native_len_prefixed_str",
        "reddb_file::decode_native_len_prefixed_bytes",
        "reddb_file::decode_native_len_prefixed_string",
        "reddb_file::encode_native_paged_collection_root",
        "reddb_file::decode_native_paged_collection_root",
        "reddb_file::encode_native_paged_cross_ref",
        "reddb_file::decode_native_paged_cross_ref",
    ] {
        assert!(
            impl_pages.contains(required),
            "UnifiedStore entity record framing should route through {required}"
        );
    }

    for forbidden in [
        "fn write_string(buf: &mut Vec<u8>, value: &str) {\n    buf.extend_from_slice",
        "fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {\n    buf.extend_from_slice",
        "fn read_string(data: &[u8], pos: &mut usize) -> Result<String, StoreError> {\n    let len = read_u32",
        "fn read_bytes(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, StoreError> {\n    let len = read_u32",
        "meta_data.extend_from_slice(&(name.len() as u32).to_le_bytes());\n            meta_data.extend_from_slice(name.as_bytes());\n            meta_data.extend_from_slice(&root_page.to_le_bytes());",
        "meta_data.extend_from_slice(&source_id.raw().to_le_bytes());",
        "meta_data.extend_from_slice(&target_id.raw().to_le_bytes());",
        "let source_id = u64::from_le_bytes([",
        "let target_id = u64::from_le_bytes([",
    ] {
        assert!(
            !impl_pages.contains(forbidden),
            "metadata length-prefixed primitives should route through reddb-file, found {forbidden:?}"
        );
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
    let physical_metadata_types =
        read(root.join("crates/reddb-file/src/physical_metadata/types.rs"));
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
        physical_metadata.contains("pub const PHYSICAL_METADATA_PROTOCOL_VERSION")
            || physical_metadata_types.contains("pub const PHYSICAL_METADATA_PROTOCOL_VERSION"),
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
