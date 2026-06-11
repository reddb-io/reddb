use crate::common::*;

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
        "[magic",
        "[version",
        "[crc32",
        "b\"RDLW\"",
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
    let runtime = read(root.join("crates/reddb-server/src/runtime/impl_primary_replica_file.rs"));
    let relay_test = read(root.join("crates/reddb-server/tests/primary_replica_relay_runtime.rs"));

    assert!(
        !runtime.contains("relay-{start_lsn:020}-{end_lsn:020}.redwal"),
        "relay segment names are a reddb_file::layout contract"
    );
    assert!(
        !relay_test.contains("relay-00000000000000000002-00000000000000000002.redwal"),
        "server relay tests should not rebuild relay segment names"
    );
    assert!(
        runtime.contains("reddb_file::layout::relay_segment_relative_path")
            && relay_test.contains("reddb_file::layout::relay_segment_relative_path"),
        "runtime should route relay segment names through reddb-file"
    );
}

#[test]
fn primary_replica_crash_contract_tests_live_in_reddb_file() {
    let root = repo_root();
    let primary_replica = read(root.join("crates/reddb-file/src/primary_replica/mod.rs"));

    for required in [
        "basebackup_manifest_ignores_leftover_tmp_file_after_crash",
        "replication_slot_catalog_ignores_leftover_tmp_file_after_crash",
        "relay_and_timeline_manifests_ignore_leftover_tmp_files_after_crash",
    ] {
        assert!(
            primary_replica.contains(required),
            "primary-replica file crash contract test should live in reddb-file: {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_backup_temp_json_names() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/archiver.rs"));
    let non_test = text
        .split("#[cfg(test)]")
        .next()
        .expect("archiver has non-test source");

    for forbidden in [
        "format!(\"-{start}-{end}\")",
        "\"{prefix}-{}{}-{}.json\"",
        "\"{prefix}-{process_id}-{start_lsn}-{end_lsn}-{nanos}.json\"",
        "std::fs::write(&temp",
        "std::fs::read(&temp",
        "std::fs::remove_file(&temp",
        "fn temp_json_path(",
        "reddb-json-object",
        "reddb-json-object-read",
        "reddb-archived-change-records",
        "reddb-archived-change-records-read",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "backup temp JSON lifecycle is a reddb-file contract, found {forbidden:?}"
        );
    }

    assert!(
        text.contains("reddb_file::BackupTempJsonFile"),
        "backup temp JSON lifecycle should route through reddb-file"
    );
    for required in [
        "BackupTempJsonFile::json_object",
        "BackupTempJsonFile::json_object_read",
        "BackupTempJsonFile::archived_change_records",
        "BackupTempJsonFile::archived_change_records_read",
    ] {
        assert!(
            non_test.contains(required),
            "backup temp JSON lifecycle should route through reddb-file constructor {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_serverless_roots_and_cache() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/runtime/impl_serverless.rs"));
    let runtime_test = read(root.join("crates/reddb-server/tests/serverless_file_runtime.rs"));
    let test_support = read(root.join("crates/reddb-server/tests/support/primary_replica_file.rs"));

    for forbidden in [
        "with_extension(\"serverless\")",
        "file_stem()",
        ".join(\"cache\")",
        "collection-data.redpack",
    ] {
        assert!(
            !text.contains(forbidden),
            "serverless root/cache filename contracts belong in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in ["with_extension(\"serverless\")", ".join(\"cache\")"] {
        assert!(
            !runtime_test.contains(forbidden),
            "serverless runtime tests should route layout through reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::ServerlessFilePlan::for_data_path",
        ".for_generation(generation)",
        ".local_cache()",
        ".collection_data_extent_ref(",
    ] {
        assert!(
            text.contains(required),
            "serverless runtime should route through {required}"
        );
    }
    assert!(
        test_support.contains("reddb_file::layout::serverless_root"),
        "test cleanup should route serverless root cleanup through reddb-file layout"
    );
}

#[test]
fn server_uses_reddb_file_for_local_backend_temp_names() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/backend/local.rs"));

    for forbidden in [
        ".cas.lock",
        ".tmp-{}-{unique}",
        "with_file_name(format!",
        "fs::copy(",
        "fs::rename(",
        "sync_all()",
    ] {
        assert!(
            !text.contains(forbidden),
            "local backend temporary filename contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::layout::local_cas_lock_path",
        "reddb_file::local_backend_download",
        "reddb_file::local_backend_atomic_upload",
    ] {
        assert!(
            text.contains(required),
            "local backend should route temporary filenames through {required}"
        );
    }
}

#[test]
fn server_does_not_own_serverless_writer_lease_artifact() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/lease.rs"));
    let failover = read(root.join("crates/reddb-server/src/server/handlers_failover.rs"));

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
        "std::fs::read(&temp",
        "std::fs::write(&temp",
        "std::fs::remove_file(&temp",
    ] {
        assert!(
            !text.contains(forbidden),
            "serverless writer lease artifact contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for forbidden in [
        "leases/{database_key}.lease.json",
        "leases/.{database_key}.lease.json.cas.lock",
    ] {
        assert!(
            !failover.contains(forbidden),
            "failover tests should route serverless writer lease artifact names through reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "pub use reddb_file::ServerlessWriterLease as WriterLease",
        "reddb_file::serverless_writer_lease_key",
        "reddb_file::ServerlessWriterLeaseTempFile",
        "reddb_file::encode_serverless_writer_lease_json",
        "reddb_file::decode_serverless_writer_lease_json",
    ] {
        assert!(
            text.contains(required),
            "serverless writer lease runtime should route through {required}"
        );
    }

    assert!(
        failover.contains("reddb_file::serverless_writer_lease_key"),
        "failover cleanup should route serverless writer lease object key through reddb-file"
    );
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
    // The server's `turboquant/snapshot.rs` re-export alias was deleted (commit
    // 8a2e6fc6); `runtime/vector_turbo_kind.rs` is now the sole consumer and
    // imports the snapshot codec directly from reddb-file.
    let text = read(root.join("crates/reddb-server/src/runtime/vector_turbo_kind.rs"));

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
        text.contains("reddb_file::{")
            && text.contains("read_turboquant_snapshot")
            && text.contains("write_turboquant_snapshot"),
        "server turboquant consumer should import the snapshot codec from reddb-file"
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
            "with_extension(\"dwb\")",
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

    let runtime = read(root.join("crates/reddb-server/src/runtime/impl_core.rs"));
    assert!(
        !runtime.contains("with_extension(\"result-cache.l2\")")
            && !runtime.contains("\"result-cache.l2\""),
        "result cache L2 path belongs in reddb-file layout"
    );
    assert!(
        runtime.contains("reddb_file::layout::result_cache_l2_path"),
        "runtime should route result cache L2 path through reddb-file"
    );

    let l2 = read(root.join("crates/reddb-server/src/storage/cache/blob/l2.rs"));
    for required in [
        "reddb_file::{",
        "blob_cache_control_path",
        "blob_cache_double_write_path",
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
    let runtime = read(root.join("crates/reddb-server/src/runtime/impl_primary_replica_file.rs"));
    let open = read(root.join("crates/reddb-server/src/storage/unified/devx/reddb/impl_core_a.rs"));
    let combined = format!("{text}\n{runtime}\n{open}");

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
        "pub fn rebootstrap_staging_root_for",
        "pub fn rebootstrap_pending_path_for",
        "pub fn rebootstrap_ready_marker_path_for",
        "pub fn rebootstrap_intent_log_path_for",
        "pub fn rebootstrap_previous_path_for",
        "pub fn write_rebootstrap_ready_marker",
        "pub fn read_rebootstrap_ready_marker",
    ] {
        assert!(
            !combined.contains(forbidden),
            "replica rebootstrap artifact contracts belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::layout::rebootstrap_staging_root",
        "reddb_file::layout::rebootstrap_pending_path",
        "reddb_file::layout::rebootstrap_ready_marker_path",
        "reddb_file::layout::rebootstrap_intent_log_path",
        "reddb_file::write_rebootstrap_ready_marker",
        "reddb_file::read_rebootstrap_ready_marker",
    ] {
        assert!(
            combined.contains(required),
            "replica rebootstrap pathing should route through {required}"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_replica_rebootstrap_lifecycle() {
    let root = repo_root();
    let lifecycle =
        read(root.join("crates/reddb-server/src/storage/unified/devx/reddb/impl_core_a.rs"));
    let listener = read(root.join("crates/reddb-server/src/wire/listener.rs"));
    let grpc = read(root.join("crates/reddb-server/src/grpc.rs"));

    for forbidden in [
        "std::fs::rename(data_path",
        "std::fs::rename(&pending",
        "std::fs::remove_file(&marker)",
        "std::fs::remove_dir_all(crate::replication::replica::rebootstrap_staging_root_for",
    ] {
        assert!(
            !lifecycle.contains(forbidden),
            "replica rebootstrap promotion belongs in reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "reddb_file::discard_ready_rebootstrap_marker",
        "reddb_file::promote_rebootstrap_pending_database",
    ] {
        assert!(
            lifecycle.contains(required),
            "replica rebootstrap lifecycle should route through {required}"
        );
    }

    for text in [listener, grpc] {
        assert!(
            text.contains("reddb_file::cleanup_rebootstrap_artifacts(data_path)"),
            "test cleanup should route replica rebootstrap artifact cleanup through reddb-file"
        );
        assert!(
            !text.contains("rebootstrap_pending_path_for(data_path)")
                && !text.contains("rebootstrap_ready_marker_path_for(data_path)")
                && !text.contains("rebootstrap_intent_log_path_for(data_path)")
                && !text.contains("rebootstrap_previous_path_for(data_path)"),
            "test cleanup should not enumerate replica rebootstrap sidecars in server"
        );
    }
}

#[test]
fn server_uses_reddb_file_for_replica_basebackup_chunk_files() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/replication/replica.rs"));
    let non_test = text
        .split("#[cfg(test)]")
        .next()
        .expect("replica.rs has non-test source");

    for forbidden in [
        "fn write_chunk_atomically",
        "OpenOptions::new()",
        "fs::create_dir_all",
        "fs::rename",
        "std::fs::read(&path)",
        "std::fs::remove_file(path)",
        "reddb_file::layout::atomic_temp_path",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "replica basebackup chunk files belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in ["stage_chunk_part", "recover_staged_chunk_parts"] {
        assert!(
            non_test.contains(required),
            "replica basebackup chunk handling should route through reddb-file {required}"
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
            "pub use slots::{ReplicationSlot",
            "pub use reddb_file::{ReplicationSlot",
            "ReplicationSlotInvalidationCause as SlotInvalidationCause",
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
fn server_primary_replica_wal_segments_are_file_owned() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/replication/primary.rs",
        "crates/reddb-server/src/runtime/impl_primary_replica_file.rs",
        "crates/reddb-server/src/runtime/impl_backup.rs",
        "crates/reddb-server/src/server/handlers_replication.rs",
        "crates/reddb-server/src/grpc/service_impl.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        let non_test = if file == "crates/reddb-server/src/replication/primary.rs" {
            text.as_str()
        } else {
            non_test_source(&text)
        };
        for forbidden in [
            ".redwal",
            "redwal",
            "wal_segment_path(",
            "existing_wal_segments",
            "removable_segments.push",
            "let mut removable_segments",
            "fs::read_dir",
            "std::fs::read_dir",
            "extension()",
        ] {
            assert!(
                !non_test.contains(forbidden),
                "{file} must let reddb-file own primary-replica WAL segment contracts, found {forbidden:?}"
            );
        }
    }

    let primary = read(root.join("crates/reddb-server/src/replication/primary.rs"));
    assert!(
        primary.contains("plan.append_wal_record("),
        "primary runtime should append primary-replica WAL through reddb-file"
    );

    let runtime = read(root.join("crates/reddb-server/src/runtime/impl_primary_replica_file.rs"));
    let runtime_non_test = non_test_source(&runtime);
    for required in [
        "plan.plan_wal_retention(&catalog, current_lsn)",
        "plan.prune_wal_segments(&catalog, current_lsn)",
    ] {
        assert!(
            runtime_non_test.contains(required),
            "runtime should delegate primary-replica WAL retention/pruning through {required}"
        );
    }

    let file_wal = read(root.join("crates/reddb-file/src/primary_replica/wal.rs"));
    for required in [
        "pub fn append_wal_record",
        "pub fn plan_wal_retention",
        "pub fn prune_wal_segments",
        "fn existing_wal_segments",
        "fs::read_dir(&wal_dir)",
        "ext.to_str()) != Some(\"redwal\")",
        "fs::remove_file(path)",
    ] {
        assert!(
            file_wal.contains(required),
            "reddb-file should own primary-replica WAL segment behavior {required}"
        );
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
fn server_ai_model_cache_uses_file_owned_manifest_contract() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/server/handlers_ai_model_cache.rs"));
    let file = read(root.join("crates/reddb-file/src/ai_model_cache.rs"));

    for forbidden in [
        "const CACHE_DIR_NAME",
        "const STAGING_DIR_NAME",
        "const PURGE_DIR_NAME",
        "const MANIFEST_FILE",
        "struct ManifestFile",
        "struct Manifest",
        "fn manifest_from_json",
        "staging_root.join(format!",
        "purge_root.join(format!",
        "model_dir.join(MANIFEST_FILE)",
        "staging_dir.join(MANIFEST_FILE)",
        "fs::copy(",
        "fs::rename(",
        "fs::write(&manifest_tmp",
        "ai_model_cache_manifest_temp_path",
        "ai_model_cache_purge_root",
        "ai_model_cache_purge_dir",
    ] {
        assert!(
            !server.contains(forbidden),
            "AI model cache file contract belongs in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "AiModelCacheManifest as Manifest",
        "AiModelCacheManifestFile as ManifestFile",
        "ai_model_cache_root",
        "ai_model_cache_staging_dir",
        "ai_model_cache_manifest_path",
        "copy_ai_model_cache_artifact",
        "encode_ai_model_cache_manifest_json",
        "decode_ai_model_cache_manifest_json",
        "write_ai_model_cache_manifest",
        "promote_ai_model_cache_staging",
        "drop_ai_model_cache_dir",
    ] {
        assert!(
            server.contains(required),
            "server AI model cache runtime should route through reddb-file {required}"
        );
    }

    for required in [
        "pub struct AiModelCacheManifestFile",
        "pub struct AiModelCacheManifest",
        "pub fn encode_ai_model_cache_manifest_json",
        "pub fn decode_ai_model_cache_manifest_json",
        "pub fn ai_model_cache_manifest_path",
        "pub fn copy_ai_model_cache_artifact",
        "pub fn write_ai_model_cache_manifest",
        "pub fn promote_ai_model_cache_staging",
        "pub fn drop_ai_model_cache_dir",
        "pub const AI_MODEL_CACHE_MANIFEST_FILE",
    ] {
        assert!(
            file.contains(required),
            "reddb-file should own AI model cache file contract {required}"
        );
    }
}
