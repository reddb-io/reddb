//! PLAN.md Phase 4 — Backup and restore contract.
//!
//! Asserts:
//!   1. A snapshot uploaded by `archive_snapshot` carries a SHA-256
//!      in its companion manifest.
//!   2. PITR restore from a clean snapshot+manifest succeeds.
//!   3. PITR restore from a snapshot whose bytes were corrupted after
//!      upload (manifest still matches the original) fails closed
//!      with a clear error before the destination database is
//!      opened.
//!   4. Manifests written before the checksum field was introduced
//!      (`snapshot_sha256: None`) still restore — backward compat.

use reddb::storage::backend::LocalBackend;
use reddb::storage::wal::{
    archive_snapshot, publish_snapshot_manifest, PointInTimeRecovery, SnapshotManifest,
};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

fn temp_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reddb-backup-{prefix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_dummy_snapshot(dir: &std::path::Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn archived_snapshot_manifest_carries_sha256() {
    let work = temp_dir("manifest-sha");
    let snapshot = write_dummy_snapshot(&work, "000001-100.snapshot", b"snapshot bytes v1");
    let snapshot_prefix = work.join("snapshots/").to_string_lossy().to_string();

    let key = archive_snapshot(&LocalBackend, &snapshot, 1, &snapshot_prefix).unwrap();
    let computed = SnapshotManifest::compute_snapshot_sha256(&snapshot).unwrap();
    let manifest = SnapshotManifest {
        timeline_id: "main".into(),
        snapshot_key: key.clone(),
        snapshot_id: 1,
        snapshot_time: 100,
        base_lsn: 0,
        schema_version: 1,
        format_version: 1,
        snapshot_sha256: Some(computed.clone()),
    };
    publish_snapshot_manifest(&LocalBackend, &manifest).unwrap();

    let loaded = reddb::storage::wal::load_snapshot_manifest(&LocalBackend, &key)
        .unwrap()
        .expect("manifest");
    assert_eq!(loaded.snapshot_sha256.as_deref(), Some(computed.as_str()));
}

#[test]
fn pitr_restore_succeeds_from_clean_snapshot() {
    let work = temp_dir("clean-restore");
    let snapshot_dir = work.join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let restore_path = work.join("restore").join("data.rdb");

    // Use an empty RedDB on the snapshot path so PITR's RedDB::open
    // succeeds against a real engine file. The snapshot bytes are
    // copied verbatim into the destination, so we can pre-stage one
    // by opening + flushing.
    let snapshot = snapshot_dir.join("000001-100.snapshot");
    reddb::storage::RedDB::open(&snapshot).unwrap().flush().unwrap();

    let computed = SnapshotManifest::compute_snapshot_sha256(&snapshot).unwrap();
    publish_snapshot_manifest(
        &LocalBackend,
        &SnapshotManifest {
            timeline_id: "main".into(),
            snapshot_key: snapshot.to_string_lossy().to_string(),
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: 1,
            format_version: 1,
            snapshot_sha256: Some(computed),
        },
    )
    .unwrap();

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        work.join("wal").to_string_lossy().to_string(),
    );
    let result = recovery
        .restore_to(150, &restore_path)
        .expect("clean restore must succeed");
    assert_eq!(result.snapshot_used, 1);
    assert!(restore_path.exists(), "restore destination must exist");
}

#[test]
fn pitr_restore_fails_closed_when_snapshot_bytes_corrupted() {
    let work = temp_dir("corrupt-restore");
    let snapshot_dir = work.join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let restore_path = work.join("restore").join("data.rdb");

    let snapshot = snapshot_dir.join("000001-100.snapshot");
    reddb::storage::RedDB::open(&snapshot).unwrap().flush().unwrap();

    // Hash the *original* bytes for the manifest, then corrupt the
    // file on disk. The manifest now references a hash that no longer
    // matches what the backend will serve back.
    let original_hash = SnapshotManifest::compute_snapshot_sha256(&snapshot).unwrap();
    publish_snapshot_manifest(
        &LocalBackend,
        &SnapshotManifest {
            timeline_id: "main".into(),
            snapshot_key: snapshot.to_string_lossy().to_string(),
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: 1,
            format_version: 1,
            snapshot_sha256: Some(original_hash),
        },
    )
    .unwrap();

    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&snapshot)
            .unwrap();
        // Flip a single byte deep inside the file. If the file is
        // shorter than 64 bytes, fall back to flipping the last byte.
        let len = f.metadata().unwrap().len();
        let target_offset = if len > 64 { 64 } else { len.saturating_sub(1) };
        f.seek(SeekFrom::Start(target_offset)).unwrap();
        let mut byte = [0u8; 1];
        use std::io::Read;
        f.read_exact(&mut byte).unwrap();
        f.seek(SeekFrom::Start(target_offset)).unwrap();
        f.write_all(&[byte[0] ^ 0x55]).unwrap();
        f.sync_all().unwrap();
    }

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        work.join("wal").to_string_lossy().to_string(),
    );
    let err = recovery
        .restore_to(150, &restore_path)
        .expect_err("corrupted snapshot must fail closed");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("integrity") || msg.contains("sha256") || msg.contains("checksum"),
        "expected integrity error, got: {msg}"
    );
}

#[test]
fn pitr_restore_proceeds_when_manifest_predates_checksum_field() {
    // Backwards-compat: a manifest written before the
    // snapshot_sha256 field existed serializes with no checksum
    // (snapshot_sha256: None). Restore must still succeed (with a
    // logged warning), so existing dev backups don't suddenly become
    // unrestorable after upgrade.
    let work = temp_dir("legacy-manifest");
    let snapshot_dir = work.join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let restore_path = work.join("restore").join("data.rdb");

    let snapshot = snapshot_dir.join("000001-100.snapshot");
    reddb::storage::RedDB::open(&snapshot).unwrap().flush().unwrap();

    publish_snapshot_manifest(
        &LocalBackend,
        &SnapshotManifest {
            timeline_id: "main".into(),
            snapshot_key: snapshot.to_string_lossy().to_string(),
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: 1,
            format_version: 1,
            snapshot_sha256: None,
        },
    )
    .unwrap();

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        work.join("wal").to_string_lossy().to_string(),
    );
    let result = recovery
        .restore_to(150, &restore_path)
        .expect("legacy manifest must restore with warning, not fail");
    assert_eq!(result.snapshot_used, 1);
}
