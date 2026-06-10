//! WAL Archiver — copies WAL segments to remote backend before truncation.
//!
//! Enables Point-in-Time Recovery (PITR) by preserving WAL history.
//! Integrates with the checkpoint flow to archive segments before they are truncated.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::replication::cdc::ChangeRecord;
use crate::storage::backend::{BackendError, RemoteBackend};
pub use reddb_file::{
    is_archived_wal_segment_key, is_backup_manifest_sidecar_key, parse_archived_wal_segment_key,
    snapshot_manifest_key, unified_manifest_key, wal_segment_manifest_key, BackupHead,
    SnapshotManifest, UnifiedManifest, UnifiedSnapshotEntry, UnifiedWalEntry, WalSegmentManifest,
    WalSegmentMeta,
};

/// Stream-hash a local file to a hex SHA-256. Shared by snapshot and
/// WAL segment archival. Streamed in 8 KiB chunks so multi-GiB files
/// don't peak memory.
pub fn sha256_file_hex(path: &Path) -> Result<String, BackendError> {
    reddb_file::sha256_file_hex(path)
        .map_err(|err| BackendError::Internal(format!("hash file {path:?}: {err}")))
}

/// Compute SHA-256 over a byte slice and return the hex digest.
/// Convenience for in-memory payloads (logical WAL segment buffer
/// before upload).
pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    reddb_file::sha256_bytes_hex(bytes)
}

/// WAL Archiver — copies WAL segments to a remote backend.
pub struct WalArchiver {
    backend: Arc<dyn RemoteBackend>,
    prefix: String,
}

impl WalArchiver {
    /// Create a new archiver with a remote backend and key prefix.
    pub fn new(backend: Arc<dyn RemoteBackend>, prefix: impl Into<String>) -> Self {
        Self {
            backend,
            prefix: prefix.into(),
        }
    }

    /// Archive a WAL file as a named segment.
    /// Call this BEFORE truncating the WAL.
    /// `prev_hash` links this segment to the prior one in the
    /// timeline (PLAN.md Phase 11.3) — pass the sha256 of the last
    /// successfully archived segment, or `None` if this is the first
    /// segment in a fresh timeline.
    pub fn archive_segment(
        &self,
        wal_path: &Path,
        lsn_start: u64,
        lsn_end: u64,
        prev_hash: Option<String>,
    ) -> Result<WalSegmentMeta, BackendError> {
        let size_bytes = std::fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0);

        // Hash *before* upload so a torn upload (partial PUT) is
        // caught by the post-restore verification. The digest covers
        // the on-disk bytes, not whatever the backend ends up holding.
        let sha = sha256_file_hex(wal_path).ok();

        let key = reddb_file::archived_wal_segment_key(&self.prefix, lsn_start, lsn_end);

        self.backend.upload(wal_path, &key)?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let meta = WalSegmentMeta {
            key,
            lsn_start,
            lsn_end,
            created_at,
            size_bytes,
            sha256: sha,
        };
        if let Err(err) = publish_wal_segment_manifest(
            self.backend.as_ref(),
            &WalSegmentManifest::from_meta(&meta, prev_hash),
        ) {
            tracing::warn!(
                target: "reddb::backup",
                error = %err,
                segment_key = %meta.key,
                "wal segment manifest publish failed; segment archived without checksum sidecar"
            );
        }
        Ok(meta)
    }

    /// Download an archived WAL segment to a local path.
    pub fn download_segment(&self, segment_key: &str, dest: &Path) -> Result<bool, BackendError> {
        self.backend.download(segment_key, dest)
    }

    /// Delete archived segments older than the given LSN.
    /// Returns the number of segments deleted.
    pub fn cleanup_before(&self, lsn: u64) -> Result<usize, BackendError> {
        let keys = self.backend.list(&self.prefix)?;
        let mut deleted = 0usize;
        for key in keys {
            let Some((lsn_start, _)) = parse_archived_wal_segment_key(&key) else {
                continue;
            };
            if lsn_start < lsn {
                self.backend.delete(&key)?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Check if a segment exists in the remote backend.
    pub fn segment_exists(&self, segment_key: &str) -> Result<bool, BackendError> {
        self.backend.exists(segment_key)
    }

    /// Get the backend name for logging.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }
}

/// Archive a snapshot file to a remote backend.
pub fn archive_snapshot(
    backend: &dyn RemoteBackend,
    snapshot_path: &Path,
    snapshot_id: u64,
    prefix: &str,
) -> Result<String, BackendError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let key = reddb_file::archived_snapshot_key(prefix, snapshot_id, timestamp);

    backend.upload(snapshot_path, &key)?;
    Ok(key)
}

/// Atomic publish via temp+rename semantics. Per backend behaviour:
///   * Filesystem backend renames the temp key, so concurrent readers
///     see either the old manifest or the new one — never a torn one.
///   * S3-compatible backends without conditional PUT can't fully
///     guarantee that, but the fresh-temp-then-replace pattern is the
///     best the trait surface offers today. PLAN.md Phase 2.4 calls
///     out PUT-if-match as a follow-up once `RemoteBackend` grows
///     conditional methods.
pub fn publish_unified_manifest(
    backend: &dyn RemoteBackend,
    prefix: &str,
    manifest: &UnifiedManifest,
) -> Result<String, BackendError> {
    let artifact = reddb_file::unified_manifest_artifact(prefix, manifest)
        .map_err(|err| BackendError::Internal(format!("build unified manifest failed: {err}")))?;
    write_json_bytes(backend, &artifact.key, &artifact.body)?;
    Ok(artifact.key)
}

pub fn load_unified_manifest(
    backend: &dyn RemoteBackend,
    prefix: &str,
) -> Result<Option<UnifiedManifest>, BackendError> {
    let key = unified_manifest_key(prefix);
    let Some(bytes) = read_json_bytes(backend, &key)? else {
        return Ok(None);
    };
    reddb_file::decode_unified_manifest_json(&bytes)
        .map(Some)
        .map_err(|err| BackendError::Internal(format!("decode unified manifest failed: {err}")))
}

/// Build the unified manifest by listing the configured backup root
/// and reading per-artifact sidecars in parallel-safe sequence. The
/// resulting `MANIFEST.json` is published atomically (temp + rename
/// on FS, fresh-temp-then-replace on S3-compatible).
///
/// `snapshot_prefix` here is the *backup root* prefix (the parent of
/// `snapshots/` and `wal/`); the unified manifest is always written
/// at `<root>/MANIFEST.json`. When the runtime hands us a more
/// specific prefix (e.g. `snapshots/clusters/dev/`), we walk back to
/// the parent before publishing.
pub fn publish_unified_manifest_for_prefix(
    backend: &dyn RemoteBackend,
    snapshot_prefix: &str,
) -> Result<String, BackendError> {
    let root = reddb_file::backup_root_from_snapshot_prefix(snapshot_prefix);
    let snapshots = collect_unified_snapshots(backend, snapshot_prefix)?;
    let wal_root = reddb_file::backup_wal_prefix(&root);
    let wal_segments = collect_unified_wal_segments(backend, &wal_root)?;
    let manifest = UnifiedManifest::new_with_engine_version(
        env!("CARGO_PKG_VERSION"),
        snapshots,
        wal_segments,
    );
    publish_unified_manifest(backend, &root, &manifest)
}

fn collect_unified_snapshots(
    backend: &dyn RemoteBackend,
    snapshot_prefix: &str,
) -> Result<Vec<UnifiedSnapshotEntry>, BackendError> {
    let keys = backend.list(snapshot_prefix)?;
    let mut out = Vec::new();
    for key in keys {
        // Skip sidecars themselves — we only want the snapshot
        // payload keys, then we read each one's sidecar for metadata.
        if is_backup_manifest_sidecar_key(&key) {
            continue;
        }
        let Some(manifest) = load_snapshot_manifest(backend, &key)? else {
            continue;
        };
        out.push(UnifiedSnapshotEntry {
            id: manifest.snapshot_id,
            lsn: manifest.base_lsn,
            ts: manifest.snapshot_time,
            bytes: 0,
            key: manifest.snapshot_key.clone(),
            checksum: manifest.snapshot_sha256.clone(),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.ts));
    Ok(out)
}

fn collect_unified_wal_segments(
    backend: &dyn RemoteBackend,
    wal_prefix: &str,
) -> Result<Vec<UnifiedWalEntry>, BackendError> {
    let keys = backend.list(wal_prefix)?;
    let mut out = Vec::new();
    for key in keys {
        if is_backup_manifest_sidecar_key(&key) {
            continue;
        }
        if !is_archived_wal_segment_key(&key) {
            continue;
        }
        let Some(manifest) = load_wal_segment_manifest(backend, &key)? else {
            continue;
        };
        out.push(UnifiedWalEntry {
            lsn_start: manifest.lsn_start,
            lsn_end: manifest.lsn_end,
            key: manifest.key.clone(),
            bytes: manifest.size_bytes,
            checksum: manifest.sha256.clone(),
            prev_hash: manifest.prev_hash.clone(),
        });
    }
    out.sort_by_key(|w| w.lsn_start);
    Ok(out)
}

pub fn publish_wal_segment_manifest(
    backend: &dyn RemoteBackend,
    manifest: &WalSegmentManifest,
) -> Result<String, BackendError> {
    let artifact = reddb_file::wal_segment_manifest_artifact(manifest).map_err(|err| {
        BackendError::Internal(format!("build wal segment manifest failed: {err}"))
    })?;
    write_json_bytes(backend, &artifact.key, &artifact.body)?;
    Ok(artifact.key)
}

pub fn load_wal_segment_manifest(
    backend: &dyn RemoteBackend,
    segment_key: &str,
) -> Result<Option<WalSegmentManifest>, BackendError> {
    let key = wal_segment_manifest_key(segment_key);
    let Some(bytes) = read_json_bytes(backend, &key)? else {
        return Ok(None);
    };
    reddb_file::decode_wal_segment_manifest_json(&bytes)
        .map(Some)
        .map_err(|err| BackendError::Internal(format!("decode wal segment manifest failed: {err}")))
}

pub fn publish_backup_head(
    backend: &dyn RemoteBackend,
    head_key: &str,
    head: &BackupHead,
) -> Result<(), BackendError> {
    let artifact = reddb_file::backup_head_artifact(head_key, head)
        .map_err(|err| BackendError::Internal(format!("build backup head failed: {err}")))?;
    write_json_bytes(backend, &artifact.key, &artifact.body)
}

pub fn load_backup_head(
    backend: &dyn RemoteBackend,
    head_key: &str,
) -> Result<Option<BackupHead>, BackendError> {
    let Some(bytes) = read_json_bytes(backend, head_key)? else {
        return Ok(None);
    };
    reddb_file::decode_backup_head_json(&bytes)
        .map(Some)
        .map_err(|err| BackendError::Internal(format!("decode backup head failed: {err}")))
}

pub fn publish_snapshot_manifest(
    backend: &dyn RemoteBackend,
    manifest: &SnapshotManifest,
) -> Result<String, BackendError> {
    let artifact = reddb_file::snapshot_manifest_artifact(manifest)
        .map_err(|err| BackendError::Internal(format!("build snapshot manifest failed: {err}")))?;
    write_json_bytes(backend, &artifact.key, &artifact.body)?;
    Ok(artifact.key)
}

pub fn load_snapshot_manifest(
    backend: &dyn RemoteBackend,
    snapshot_key: &str,
) -> Result<Option<SnapshotManifest>, BackendError> {
    let key = snapshot_manifest_key(snapshot_key);
    let Some(bytes) = read_json_bytes(backend, &key)? else {
        return Ok(None);
    };
    reddb_file::decode_snapshot_manifest_json(&bytes)
        .map(Some)
        .map_err(|err| BackendError::Internal(format!("decode snapshot manifest failed: {err}")))
}

pub fn archive_change_records(
    backend: &dyn RemoteBackend,
    prefix: &str,
    records: &[(u64, Vec<u8>)],
    prev_hash: Option<String>,
) -> Result<Option<WalSegmentMeta>, BackendError> {
    let Some((lsn_start, _)) = records.first() else {
        return Ok(None);
    };
    let Some((lsn_end, _)) = records.last() else {
        return Ok(None);
    };

    let body = reddb_file::encode_archived_logical_wal_records(records).map_err(|err| {
        BackendError::Internal(format!("encode archived logical wal failed: {err}"))
    })?;
    // Hash the encoded payload before persisting so the digest
    // matches what gets uploaded byte-for-byte.
    let sha = sha256_bytes_hex(&body);

    let temp = reddb_file::BackupTempJsonFile::archived_change_records(*lsn_start, *lsn_end);
    let size_bytes = temp
        .write_bytes(&body)
        .map_err(|err| BackendError::Transport(format!("write temp logical wal failed: {err}")))?;

    let key = reddb_file::archived_wal_segment_key(prefix, *lsn_start, *lsn_end);
    backend.upload(temp.path(), &key)?;

    let meta = WalSegmentMeta {
        key,
        lsn_start: *lsn_start,
        lsn_end: *lsn_end,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        size_bytes,
        sha256: Some(sha),
    };

    // Per-segment sidecar manifest. Best-effort — a failure to publish
    // the manifest leaves the segment without a checksum, which restore
    // tolerates with a warning, so this is non-fatal. We still log so
    // operators can flag that backup integrity coverage is degraded.
    if let Err(err) =
        publish_wal_segment_manifest(backend, &WalSegmentManifest::from_meta(&meta, prev_hash))
    {
        tracing::warn!(
            target: "reddb::backup",
            error = %err,
            segment_key = %meta.key,
            "wal segment manifest publish failed; segment archived without checksum sidecar"
        );
    }

    Ok(Some(meta))
}

pub fn load_archived_change_records(
    backend: &dyn RemoteBackend,
    segment_key: &str,
) -> Result<Vec<ChangeRecord>, BackendError> {
    let (records, _digest) = load_archived_change_records_with_sha256(backend, segment_key)?;
    Ok(records)
}

/// Same as `load_archived_change_records` but also returns the
/// SHA-256 of the downloaded payload bytes so the caller can verify
/// it against the segment's manifest digest. Restore flows pair this
/// with `load_wal_segment_manifest` to fail closed on tampering.
pub fn load_archived_change_records_with_sha256(
    backend: &dyn RemoteBackend,
    segment_key: &str,
) -> Result<(Vec<ChangeRecord>, Option<String>), BackendError> {
    let temp = reddb_file::BackupTempJsonFile::archived_change_records_read();
    let found = backend.download(segment_key, temp.path())?;
    if !found {
        return Ok((Vec::new(), None));
    }
    let bytes = temp
        .read_bytes()
        .map_err(|err| BackendError::Transport(format!("read temp logical wal failed: {err}")))?;
    let digest = sha256_bytes_hex(&bytes);

    let archived = reddb_file::decode_archived_logical_wal_records(&bytes).map_err(|err| {
        BackendError::Internal(format!("decode archived logical wal failed: {err}"))
    })?;
    let mut out = Vec::new();
    for entry in archived {
        let record = ChangeRecord::decode(&entry.data)
            .map_err(|err| BackendError::Internal(format!("decode wal record failed: {err}")))?;
        out.push(record);
    }
    Ok((out, Some(digest)))
}

fn write_json_bytes(
    backend: &dyn RemoteBackend,
    key: &str,
    bytes: &[u8],
) -> Result<(), BackendError> {
    let temp = reddb_file::BackupTempJsonFile::json_object();
    temp.write_bytes(bytes)
        .map_err(|err| BackendError::Transport(format!("write temp json object failed: {err}")))?;
    backend.upload(temp.path(), key)
}

fn read_json_bytes(
    backend: &dyn RemoteBackend,
    key: &str,
) -> Result<Option<Vec<u8>>, BackendError> {
    let temp = reddb_file::BackupTempJsonFile::json_object_read();
    let found = backend.download(key, temp.path())?;
    if !found {
        return Ok(None);
    }
    let bytes = temp
        .read_bytes()
        .map_err(|err| BackendError::Transport(format!("read temp json object failed: {err}")))?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::local::LocalBackend;
    use std::io::Write;

    #[test]
    fn test_archive_and_download() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-archiver-test-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend_dir = temp_dir.join("backend");
        let _ = std::fs::create_dir_all(&backend_dir);

        let backend = Arc::new(LocalBackend);
        // Anchor the archive prefix inside the temp dir. `backup_wal_prefix("")`
        // yields a cwd-relative `wal/` directory, which leaks archived segments
        // into the working tree (`crates/reddb-server/wal/`) and is never cleaned.
        let wal_prefix = reddb_file::backup_wal_prefix(&format!("{}/", temp_dir.to_string_lossy()));
        let archiver = WalArchiver::new(backend, &wal_prefix);

        // Create a fake WAL file
        let wal_path = reddb_file::layout::wal_component_temp_path(
            &temp_dir,
            "archiver",
            "source",
            std::process::id(),
        );
        {
            let mut f = std::fs::File::create(&wal_path).unwrap();
            f.write_all(b"fake wal data").unwrap();
        }

        // Archive it
        let meta = archiver.archive_segment(&wal_path, 8, 500, None).unwrap();
        assert_eq!(meta.lsn_start, 8);
        assert_eq!(meta.lsn_end, 500);
        assert!(meta.key.starts_with(&wal_prefix));
        assert!(is_archived_wal_segment_key(&meta.key));

        // Download it
        let dest = reddb_file::layout::wal_component_temp_path(
            &temp_dir,
            "archiver",
            "downloaded",
            std::process::id(),
        );
        let found = archiver.download_segment(&meta.key, &dest).unwrap();
        assert!(found);
        assert!(dest.exists());

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn test_backup_head_roundtrip() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-backup-head-test-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend = LocalBackend;
        let root_prefix = format!("{}/", temp_dir.to_string_lossy());
        let head_key = reddb_file::backup_head_key(&root_prefix);

        let head = BackupHead {
            timeline_id: "main".to_string(),
            snapshot_key: reddb_file::archived_snapshot_key(
                &reddb_file::backup_snapshot_prefix(""),
                1,
                123,
            ),
            snapshot_id: 1,
            snapshot_time: 123,
            current_lsn: 456,
            last_archived_lsn: 456,
            wal_prefix: reddb_file::backup_wal_prefix(""),
        };

        publish_backup_head(&backend, &head_key, &head).unwrap();
        let loaded = load_backup_head(&backend, &head_key)
            .unwrap()
            .expect("head");
        assert_eq!(loaded, head);

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn test_snapshot_manifest_roundtrip() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-snapshot-manifest-test-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend = LocalBackend;
        let root_prefix = format!("{}/", temp_dir.to_string_lossy());
        let manifest = SnapshotManifest {
            timeline_id: "main".to_string(),
            snapshot_key: reddb_file::archived_snapshot_key(
                &reddb_file::backup_snapshot_prefix(&root_prefix),
                1,
                123,
            ),
            snapshot_id: 1,
            snapshot_time: 123,
            base_lsn: 456,
            schema_version: crate::api::REDDB_FORMAT_VERSION,
            format_version: crate::api::REDDB_FORMAT_VERSION,
            snapshot_sha256: None,
        };

        publish_snapshot_manifest(&backend, &manifest).unwrap();
        let loaded = load_snapshot_manifest(&backend, &manifest.snapshot_key)
            .unwrap()
            .expect("manifest");
        assert_eq!(loaded, manifest);

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn test_archived_change_records_roundtrip() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-archived-change-records-test-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend = LocalBackend;
        let root_prefix = format!("{}/", temp_dir.to_string_lossy());
        let prefix = reddb_file::backup_wal_prefix(&root_prefix);
        let record = ChangeRecord {
            term: crate::replication::DEFAULT_REPLICATION_TERM,
            lsn: 7,
            timestamp: 1234,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 42,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        };

        let meta =
            archive_change_records(&backend, &prefix, &[(record.lsn, record.encode())], None)
                .unwrap()
                .expect("meta");
        let loaded = load_archived_change_records(&backend, &meta.key).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].lsn, 7);

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn archive_change_records_writes_sidecar_with_sha256() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-archiver-sidecar-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend = LocalBackend;
        let root_prefix = format!("{}/", temp_dir.to_string_lossy());
        let prefix = reddb_file::backup_wal_prefix(&root_prefix);
        let record = ChangeRecord {
            term: crate::replication::DEFAULT_REPLICATION_TERM,
            lsn: 11,
            timestamp: 99,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 1,
            entity_kind: "row".to_string(),
            entity_bytes: Some(b"x".to_vec()),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        };
        let meta =
            archive_change_records(&backend, &prefix, &[(record.lsn, record.encode())], None)
                .unwrap()
                .expect("meta");
        assert!(meta.sha256.is_some(), "WalSegmentMeta should carry sha256");

        let sidecar = load_wal_segment_manifest(&backend, &meta.key)
            .unwrap()
            .expect("sidecar");
        assert_eq!(sidecar.key, meta.key);
        assert_eq!(sidecar.lsn_start, meta.lsn_start);
        assert_eq!(sidecar.lsn_end, meta.lsn_end);
        assert_eq!(sidecar.sha256, meta.sha256);

        let (_records, computed) =
            load_archived_change_records_with_sha256(&backend, &meta.key).unwrap();
        assert_eq!(computed, meta.sha256, "computed sha must match sidecar");

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn unified_manifest_json_roundtrip() {
        let manifest = UnifiedManifest::new(
            vec![UnifiedSnapshotEntry {
                id: 7,
                lsn: 100,
                ts: 1730000000000,
                bytes: 4096,
                key: reddb_file::archived_snapshot_key(
                    &reddb_file::backup_snapshot_prefix(""),
                    7,
                    1730000000000,
                ),
                checksum: Some("9f8b".to_string()),
            }],
            vec![UnifiedWalEntry {
                lsn_start: 100,
                lsn_end: 250,
                key: reddb_file::archived_wal_segment_key(
                    &reddb_file::backup_wal_prefix(""),
                    100,
                    250,
                ),
                bytes: 1024,
                checksum: Some("c1d2".to_string()),
                prev_hash: Some("9f8b".to_string()),
            }],
        );

        let json = reddb_file::encode_unified_manifest_json(&manifest).unwrap();
        let parsed = reddb_file::decode_unified_manifest_json(&json).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.latest_lsn, 250);

        // prev_hash must round-trip with `sha256:` prefix on the wire
        // (PLAN.md Phase 11.3) so external verifiers can validate
        // the chain end-to-end without parsing the per-segment sidecar.
        assert_eq!(parsed.wal_segments[0].prev_hash.as_deref(), Some("9f8b"));
        let wal_wire = String::from_utf8(json.clone()).unwrap();
        assert!(
            wal_wire.contains("\"prev_hash\":\"sha256:9f8b\""),
            "wire form must include sha256: prefix on prev_hash; got: {wal_wire}"
        );

        // Checksum should round-trip with the `sha256:` prefix in the
        // wire form but parse back to the bare hex.
        let body = String::from_utf8(json).unwrap();
        assert!(
            body.contains("\"sha256:9f8b\""),
            "wire form must include sha256: prefix"
        );
        assert_eq!(parsed.snapshots[0].checksum.as_deref(), Some("9f8b"));
    }

    #[test]
    fn unified_manifest_publish_load_roundtrip() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-unified-manifest-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let prefix = temp_dir.to_string_lossy().to_string();

        let backend = LocalBackend;
        let manifest = UnifiedManifest::new(vec![], vec![]);
        publish_unified_manifest(&backend, &prefix, &manifest).unwrap();
        let loaded = load_unified_manifest(&backend, &prefix)
            .unwrap()
            .expect("manifest");
        assert_eq!(loaded.version, "1.0");
        assert_eq!(loaded.latest_lsn, 0);

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn archive_change_records_chains_prev_hash() {
        // Auto-cleaning temp dir: removed on drop (incl. panic), so the test
        // leaves no residue even if an assertion fails.
        let dir = tempfile::Builder::new()
            .prefix("reddb-archive-chain-")
            .tempdir()
            .unwrap();
        let temp_dir = dir.path().to_path_buf();
        let backend = LocalBackend;
        let root_prefix = format!("{}/", temp_dir.to_string_lossy());
        let prefix = reddb_file::backup_wal_prefix(&root_prefix);

        let mk = |lsn: u64| ChangeRecord {
            term: crate::replication::DEFAULT_REPLICATION_TERM,
            lsn,
            timestamp: lsn * 1000,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: lsn,
            entity_kind: "row".to_string(),
            entity_bytes: Some(format!("payload-{lsn}").into_bytes()),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        };

        let r1 = mk(10);
        let m1 = archive_change_records(&backend, &prefix, &[(r1.lsn, r1.encode())], None)
            .unwrap()
            .expect("seg 1");
        let r2 = mk(11);
        let m2 = archive_change_records(
            &backend,
            &prefix,
            &[(r2.lsn, r2.encode())],
            m1.sha256.clone(),
        )
        .unwrap()
        .expect("seg 2");
        let r3 = mk(12);
        let m3 = archive_change_records(
            &backend,
            &prefix,
            &[(r3.lsn, r3.encode())],
            m2.sha256.clone(),
        )
        .unwrap()
        .expect("seg 3");

        let s1 = load_wal_segment_manifest(&backend, &m1.key)
            .unwrap()
            .unwrap();
        let s2 = load_wal_segment_manifest(&backend, &m2.key)
            .unwrap()
            .unwrap();
        let s3 = load_wal_segment_manifest(&backend, &m3.key)
            .unwrap()
            .unwrap();
        assert!(s1.prev_hash.is_none(), "first segment has no prev_hash");
        assert_eq!(s2.prev_hash, m1.sha256, "seg 2 links to seg 1 sha256");
        assert_eq!(s3.prev_hash, m2.sha256, "seg 3 links to seg 2 sha256");

        // `dir` (TempDir) auto-removes the whole tree on drop.
    }

    #[test]
    fn wal_segment_manifest_carries_prev_hash_through_json() {
        let m = WalSegmentManifest {
            key: reddb_file::archived_wal_segment_key(&reddb_file::backup_wal_prefix(""), 10, 10),
            lsn_start: 10,
            lsn_end: 10,
            size_bytes: 128,
            created_at: 1730000000000,
            sha256: Some("abc".to_string()),
            prev_hash: Some("def".to_string()),
        };
        let parsed = reddb_file::decode_wal_segment_manifest_json(
            &reddb_file::encode_wal_segment_manifest_json(&m).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn derive_backup_root_handles_typical_prefixes() {
        assert_eq!(reddb_file::backup_root_from_snapshot_prefix(""), "");
        assert_eq!(
            reddb_file::backup_root_from_snapshot_prefix("snapshots/"),
            ""
        );
        assert_eq!(
            reddb_file::backup_root_from_snapshot_prefix("snapshots"),
            ""
        );
        assert_eq!(
            reddb_file::backup_root_from_snapshot_prefix("clusters/dev/snapshots/"),
            "clusters/dev/"
        );
        assert_eq!(
            reddb_file::backup_root_from_snapshot_prefix("clusters/dev/snapshots"),
            "clusters/dev/"
        );
        assert_eq!(
            reddb_file::backup_root_from_snapshot_prefix("clusters/dev/"),
            "clusters/dev/"
        );
    }
}
