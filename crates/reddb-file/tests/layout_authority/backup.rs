use crate::common::*;

#[test]
fn server_does_not_own_backup_or_wal_archive_manifest_codecs() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/storage/wal/archiver.rs"));
    let wal_mod = read(root.join("crates/reddb-server/src/storage/wal/mod.rs"));
    let runtime_backup = read(root.join("crates/reddb-server/src/runtime/impl_backup.rs"));
    let api = read(root.join("crates/reddb-server/src/api.rs"));
    // impl_core.rs was split into runtime/impl_*.rs sub-modules; scan them all so this
    // authority check follows the code (e.g. backup_wal_prefix now lives in impl_lifecycle.rs).
    let runtime_dir = root.join("crates/reddb-server/src/runtime");
    let mut runtime_core = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(&runtime_dir)
        .expect("runtime dir readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("rs"))
        .collect();
    entries.sort();
    for path in entries {
        runtime_core.push_str(&read(path));
        runtime_core.push('\n');
    }
    let service_cli = read(root.join("crates/reddb-server/src/service_cli.rs"));
    let recovery = read(root.join("crates/reddb-server/src/storage/wal/recovery.rs"));
    let file = read(root.join("crates/reddb-file/src/backup_manifest.rs"));
    let non_test_archiver = text
        .split("#[cfg(test)]")
        .next()
        .expect("archiver has non-test source");
    let non_test_recovery = recovery
        .split("#[cfg(test)]")
        .next()
        .expect("recovery has non-test source");

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
        "{:012}-{}.snapshot",
        "encode_unified_manifest_json(manifest)",
        "encode_wal_segment_manifest_json(manifest)",
        "encode_backup_head_json(head)",
        "encode_snapshot_manifest_json(manifest)",
    ] {
        assert!(
            !non_test_archiver.contains(forbidden),
            "backup/WAL archive manifest codecs belong in reddb-file, found {forbidden:?}"
        );
    }

    for required in [
        "reddb_file::decode_unified_manifest_json",
        "reddb_file::decode_wal_segment_manifest_json",
        "reddb_file::decode_backup_head_json",
        "reddb_file::decode_snapshot_manifest_json",
        "reddb_file::unified_manifest_artifact",
        "reddb_file::wal_segment_manifest_artifact",
        "reddb_file::backup_head_artifact",
        "reddb_file::snapshot_manifest_artifact",
        "reddb_file::encode_archived_logical_wal_records",
        "reddb_file::decode_archived_logical_wal_records",
        "reddb_file::archived_wal_segment_key",
        "parse_archived_wal_segment_key",
        "is_archived_wal_segment_key",
        "is_backup_manifest_sidecar_key",
        "archived_snapshot_key",
        "backup_root_from_snapshot_prefix",
        "backup_snapshot_prefix",
        "backup_wal_prefix",
    ] {
        assert!(
            text.contains(required),
            "backup/WAL archive manifest runtime should route through {required}"
        );
    }
    for required in [
        "pub struct BackupJsonArtifact",
        "pub fn unified_manifest_artifact",
        "pub fn wal_segment_manifest_artifact",
        "pub fn backup_head_artifact",
        "pub fn snapshot_manifest_artifact",
    ] {
        assert!(
            file.contains(required),
            "backup/WAL archive manifest artifact contract should live in reddb-file: {required}"
        );
    }
    for forbidden in [
        "BackupHead",
        "SnapshotManifest",
        "UnifiedManifest",
        "UnifiedSnapshotEntry",
        "UnifiedWalEntry",
        "WalSegmentManifest",
        "WalSegmentMeta",
        "snapshot_manifest_key",
        "unified_manifest_key",
        "wal_segment_manifest_key",
        "sha256_bytes_hex",
        "sha256_file_hex",
    ] {
        assert!(
            !wal_mod.contains(forbidden),
            "storage::wal must not reexport backup manifest file contract {forbidden:?}"
        );
    }
    for forbidden in [
        "crate::storage::wal::BackupHead",
        "crate::storage::wal::SnapshotManifest",
    ] {
        assert!(
            !runtime_backup.contains(forbidden),
            "runtime backup should use reddb_file manifest types directly, found {forbidden:?}"
        );
    }
    for required in ["parse_archived_snapshot_key"] {
        assert!(
            text.contains(required) || non_test_recovery.contains(required),
            "backup snapshot archive key parsing should route through {required}"
        );
    }

    for forbidden in ["manifests/head.json", "snapshots/", "wal/"] {
        assert!(
            !api.contains(forbidden),
            "backup namespace defaults belong in reddb-file, found {forbidden:?}"
        );
    }
    assert!(
        !non_test_recovery.contains("manifests/head.json"),
        "backup head key derivation belongs in reddb-file"
    );
    for forbidden in [
        "join(\"manifests\")",
        "snapshots/000",
        "wal/000",
        "format!(\"{}/wal/\"",
    ] {
        assert!(
            !text.contains(forbidden) && !recovery.contains(forbidden),
            "backup artifact test keys should route through reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in ["1-100.snapshot", "2-200.snapshot"] {
        assert!(
            !recovery.contains(forbidden),
            "backup snapshot test keys should route through reddb-file, found {forbidden:?}"
        );
    }
    for required in [
        "reddb_file::backup_head_key",
        "reddb_file::backup_snapshot_prefix",
        "reddb_file::backup_wal_prefix",
    ] {
        assert!(
            api.contains(required),
            "API backup defaults should route through {required}"
        );
    }
    assert!(
        !runtime_core.contains("\"prefix\": \"wal/\""),
        "runtime WAL archive default prefix belongs in reddb-file"
    );
    assert!(
        runtime_core.contains("reddb_file::backup_wal_prefix(\"\")"),
        "runtime WAL archive default prefix should route through reddb-file"
    );
    assert!(
        non_test_recovery.contains("reddb_file::backup_head_key")
            && non_test_recovery.contains("reddb_file::backup_root_from_snapshot_prefix"),
        "recovery backup head lookup should route through reddb-file"
    );
    assert!(
        recovery.contains("reddb_file::backup_snapshot_dir")
            && recovery.contains("reddb_file::backup_wal_dir"),
        "recovery backup test paths should route through reddb-file"
    );
    assert!(
        recovery.contains("reddb_file::backup_snapshot_dir_prefix")
            && recovery.contains("reddb_file::backup_wal_dir_prefix"),
        "recovery local backend test prefixes should route through reddb-file"
    );
    for forbidden in ["join(\"snapshots\")", "join(\"wal\")"] {
        assert!(
            !text.contains(forbidden) && !recovery.contains(forbidden),
            "backup artifact path joins belong in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in [
        "format!(\"{}/data.rdb\"",
        "\"clusters/dev/data.rdb\".to_string()",
    ] {
        assert!(
            !service_cli.contains(forbidden),
            "remote database key defaults belong in reddb-file, found {forbidden:?}"
        );
    }
    assert!(
        service_cli.contains("reddb_file::remote_database_key"),
        "service CLI remote database key defaults should route through reddb-file"
    );

    for forbidden in [
        ".strip_suffix(\".snapshot\")",
        ".strip_suffix(\".wal\")",
        "!key.ends_with(\".wal\")",
        "key.ends_with(\".manifest.json\")",
    ] {
        assert!(
            !non_test_archiver.contains(forbidden),
            "archived WAL key parsing belongs in reddb-file, found {forbidden:?}"
        );
        assert!(
            !non_test_recovery.contains(forbidden),
            "archived WAL key parsing belongs in reddb-file, found {forbidden:?}"
        );
    }
    for forbidden in [".ends_with(\".wal\")"] {
        assert!(
            !text.contains(forbidden) && !recovery.contains(forbidden),
            "archived WAL key suffix checks belong in reddb-file, found {forbidden:?}"
        );
    }
}
