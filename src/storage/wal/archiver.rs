//! WAL Archiver — copies WAL segments to remote backend before truncation.
//!
//! Enables Point-in-Time Recovery (PITR) by preserving WAL history.
//! Integrates with the checkpoint flow to archive segments before they are truncated.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Map, Value as JsonValue};
use crate::replication::cdc::ChangeRecord;
use crate::storage::backend::{BackendError, RemoteBackend};

/// Metadata about an archived WAL segment.
#[derive(Debug, Clone)]
pub struct WalSegmentMeta {
    /// Remote key (e.g., "wal/000000000008-000000050432.wal")
    pub key: String,
    /// Starting LSN of this segment
    pub lsn_start: u64,
    /// Ending LSN of this segment
    pub lsn_end: u64,
    /// When this segment was archived (unix ms)
    pub created_at: u64,
    /// Size in bytes
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupHead {
    pub timeline_id: String,
    pub snapshot_key: String,
    pub snapshot_id: u64,
    pub snapshot_time: u64,
    pub current_lsn: u64,
    pub last_archived_lsn: u64,
    pub wal_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotManifest {
    pub timeline_id: String,
    pub snapshot_key: String,
    pub snapshot_id: u64,
    pub snapshot_time: u64,
    pub base_lsn: u64,
    pub schema_version: u32,
    pub format_version: u32,
    /// Hex-encoded SHA-256 of the snapshot bytes computed at upload
    /// time. Restore reads this from the manifest, downloads the
    /// snapshot, recomputes the hash, and refuses to proceed on a
    /// mismatch. `None` for legacy manifests written before this
    /// field was introduced — restore tolerates absence (with a
    /// warning) but rejects a present-but-wrong value.
    pub snapshot_sha256: Option<String>,
}

impl BackupHead {
    pub fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert(
            "timeline_id".to_string(),
            JsonValue::String(self.timeline_id.clone()),
        );
        object.insert(
            "snapshot_key".to_string(),
            JsonValue::String(self.snapshot_key.clone()),
        );
        object.insert(
            "snapshot_id".to_string(),
            JsonValue::Number(self.snapshot_id as f64),
        );
        object.insert(
            "snapshot_time".to_string(),
            JsonValue::Number(self.snapshot_time as f64),
        );
        object.insert(
            "current_lsn".to_string(),
            JsonValue::Number(self.current_lsn as f64),
        );
        object.insert(
            "last_archived_lsn".to_string(),
            JsonValue::Number(self.last_archived_lsn as f64),
        );
        object.insert(
            "wal_prefix".to_string(),
            JsonValue::String(self.wal_prefix.clone()),
        );
        JsonValue::Object(object)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        Ok(Self {
            timeline_id: value
                .get("timeline_id")
                .and_then(JsonValue::as_str)
                .unwrap_or("main")
                .to_string(),
            snapshot_key: value
                .get("snapshot_key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    BackendError::Internal("backup head missing snapshot_key".to_string())
                })?
                .to_string(),
            snapshot_id: value
                .get("snapshot_id")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup head missing snapshot_id".to_string())
                })?,
            snapshot_time: value
                .get("snapshot_time")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup head missing snapshot_time".to_string())
                })?,
            current_lsn: value
                .get("current_lsn")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            last_archived_lsn: value
                .get("last_archived_lsn")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            wal_prefix: value
                .get("wal_prefix")
                .and_then(JsonValue::as_str)
                .unwrap_or("wal/")
                .to_string(),
        })
    }
}

impl SnapshotManifest {
    pub fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert(
            "timeline_id".to_string(),
            JsonValue::String(self.timeline_id.clone()),
        );
        object.insert(
            "snapshot_key".to_string(),
            JsonValue::String(self.snapshot_key.clone()),
        );
        object.insert(
            "snapshot_id".to_string(),
            JsonValue::Number(self.snapshot_id as f64),
        );
        object.insert(
            "snapshot_time".to_string(),
            JsonValue::Number(self.snapshot_time as f64),
        );
        object.insert(
            "base_lsn".to_string(),
            JsonValue::Number(self.base_lsn as f64),
        );
        object.insert(
            "schema_version".to_string(),
            JsonValue::Number(self.schema_version as f64),
        );
        object.insert(
            "format_version".to_string(),
            JsonValue::Number(self.format_version as f64),
        );
        if let Some(ref sha) = self.snapshot_sha256 {
            object.insert(
                "snapshot_sha256".to_string(),
                JsonValue::String(sha.clone()),
            );
        }
        JsonValue::Object(object)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        Ok(Self {
            timeline_id: value
                .get("timeline_id")
                .and_then(JsonValue::as_str)
                .unwrap_or("main")
                .to_string(),
            snapshot_key: value
                .get("snapshot_key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    BackendError::Internal("snapshot manifest missing snapshot_key".to_string())
                })?
                .to_string(),
            snapshot_id: value
                .get("snapshot_id")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("snapshot manifest missing snapshot_id".to_string())
                })?,
            snapshot_time: value
                .get("snapshot_time")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("snapshot manifest missing snapshot_time".to_string())
                })?,
            base_lsn: value
                .get("base_lsn")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            schema_version: value
                .get("schema_version")
                .and_then(JsonValue::as_u64)
                .unwrap_or(crate::api::REDDB_FORMAT_VERSION as u64)
                as u32,
            format_version: value
                .get("format_version")
                .and_then(JsonValue::as_u64)
                .unwrap_or(crate::api::REDDB_FORMAT_VERSION as u64)
                as u32,
            snapshot_sha256: value
                .get("snapshot_sha256")
                .and_then(JsonValue::as_str)
                .map(|s| s.to_string()),
        })
    }

    /// Compute SHA-256 over the local snapshot file. Used at archive
    /// time so the manifest can carry the digest for restore-side
    /// verification. Streamed (8 KiB chunks) so very large snapshots
    /// don't peak memory.
    pub fn compute_snapshot_sha256(snapshot_path: &Path) -> Result<String, BackendError> {
        use std::fs::File;
        use std::io::Read;
        let mut hasher = crate::crypto::sha256::Sha256::new();
        let mut file = File::open(snapshot_path)
            .map_err(|err| BackendError::Internal(format!("open snapshot for hash: {err}")))?;
        let mut buf = vec![0u8; 8 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|err| BackendError::Internal(format!("read snapshot for hash: {err}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let digest = hasher.finalize();
        Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
    }
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
    pub fn archive_segment(
        &self,
        wal_path: &Path,
        lsn_start: u64,
        lsn_end: u64,
    ) -> Result<WalSegmentMeta, BackendError> {
        let size_bytes = std::fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0);

        let key = format!("{}{:012}-{:012}.wal", self.prefix, lsn_start, lsn_end);

        self.backend.upload(wal_path, &key)?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(WalSegmentMeta {
            key,
            lsn_start,
            lsn_end,
            created_at,
            size_bytes,
        })
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
            let path = PathBuf::from(&key);
            let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((start, _end)) = file_name
                .strip_suffix(".wal")
                .and_then(|base| base.split_once('-'))
            else {
                continue;
            };
            let Ok(lsn_start) = start.parse::<u64>() else {
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

    let key = format!("{}{:012}-{}.snapshot", prefix, snapshot_id, timestamp);

    backend.upload(snapshot_path, &key)?;
    Ok(key)
}

pub fn snapshot_manifest_key(snapshot_key: &str) -> String {
    format!("{snapshot_key}.manifest.json")
}

pub fn publish_backup_head(
    backend: &dyn RemoteBackend,
    head_key: &str,
    head: &BackupHead,
) -> Result<(), BackendError> {
    write_json_object(backend, head_key, &head.to_json_value())
}

pub fn load_backup_head(
    backend: &dyn RemoteBackend,
    head_key: &str,
) -> Result<Option<BackupHead>, BackendError> {
    let Some(value) = read_json_object(backend, head_key)? else {
        return Ok(None);
    };
    Ok(Some(BackupHead::from_json_value(&value)?))
}

pub fn publish_snapshot_manifest(
    backend: &dyn RemoteBackend,
    manifest: &SnapshotManifest,
) -> Result<String, BackendError> {
    let key = snapshot_manifest_key(&manifest.snapshot_key);
    write_json_object(backend, &key, &manifest.to_json_value())?;
    Ok(key)
}

pub fn load_snapshot_manifest(
    backend: &dyn RemoteBackend,
    snapshot_key: &str,
) -> Result<Option<SnapshotManifest>, BackendError> {
    let key = snapshot_manifest_key(snapshot_key);
    let Some(value) = read_json_object(backend, &key)? else {
        return Ok(None);
    };
    Ok(Some(SnapshotManifest::from_json_value(&value)?))
}

pub fn archive_change_records(
    backend: &dyn RemoteBackend,
    prefix: &str,
    records: &[(u64, Vec<u8>)],
) -> Result<Option<WalSegmentMeta>, BackendError> {
    let Some((lsn_start, _)) = records.first() else {
        return Ok(None);
    };
    let Some((lsn_end, _)) = records.last() else {
        return Ok(None);
    };

    let payload = JsonValue::Array(
        records
            .iter()
            .map(|(lsn, bytes)| {
                let mut object = Map::new();
                object.insert("lsn".to_string(), JsonValue::Number(*lsn as f64));
                object.insert("data".to_string(), JsonValue::String(hex::encode(bytes)));
                JsonValue::Object(object)
            })
            .collect(),
    );
    let temp = temp_json_path(
        "reddb-archived-change-records",
        Some(*lsn_start),
        Some(*lsn_end),
    );
    std::fs::write(
        &temp,
        crate::json::to_vec(&payload).map_err(|err| {
            BackendError::Internal(format!("encode archived logical wal failed: {err}"))
        })?,
    )
    .map_err(|err| BackendError::Transport(format!("write temp logical wal failed: {err}")))?;

    let key = format!("{}{:012}-{:012}.wal", prefix, lsn_start, lsn_end);
    backend.upload(&temp, &key)?;
    let size_bytes = std::fs::metadata(&temp).map(|meta| meta.len()).unwrap_or(0);
    let _ = std::fs::remove_file(&temp);

    Ok(Some(WalSegmentMeta {
        key,
        lsn_start: *lsn_start,
        lsn_end: *lsn_end,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        size_bytes,
    }))
}

pub fn load_archived_change_records(
    backend: &dyn RemoteBackend,
    segment_key: &str,
) -> Result<Vec<ChangeRecord>, BackendError> {
    let Some(value) = read_json_object(backend, segment_key)? else {
        return Ok(Vec::new());
    };
    let Some(entries) = value.as_array() else {
        return Err(BackendError::Internal(
            "archived logical wal must be a JSON array".to_string(),
        ));
    };
    let mut out = Vec::new();
    for entry in entries {
        let Some(data_hex) = entry.get("data").and_then(JsonValue::as_str) else {
            continue;
        };
        let data = hex::decode(data_hex).map_err(|err| {
            BackendError::Internal(format!("decode wal record hex failed: {err}"))
        })?;
        let record = ChangeRecord::decode(&data)
            .map_err(|err| BackendError::Internal(format!("decode wal record failed: {err}")))?;
        out.push(record);
    }
    Ok(out)
}

fn write_json_object(
    backend: &dyn RemoteBackend,
    key: &str,
    value: &JsonValue,
) -> Result<(), BackendError> {
    let temp = temp_json_path("reddb-json-object", None, None);
    std::fs::write(
        &temp,
        crate::json::to_vec(value)
            .map_err(|err| BackendError::Internal(format!("encode json object failed: {err}")))?,
    )
    .map_err(|err| BackendError::Transport(format!("write temp json object failed: {err}")))?;
    let upload_result = backend.upload(&temp, key);
    let _ = std::fs::remove_file(&temp);
    upload_result
}

fn read_json_object(
    backend: &dyn RemoteBackend,
    key: &str,
) -> Result<Option<JsonValue>, BackendError> {
    let temp = temp_json_path("reddb-json-object-read", None, None);
    let found = backend.download(key, &temp)?;
    if !found {
        return Ok(None);
    }
    let bytes = std::fs::read(&temp)
        .map_err(|err| BackendError::Transport(format!("read temp json object failed: {err}")))?;
    let _ = std::fs::remove_file(&temp);
    let value = crate::json::from_slice::<JsonValue>(&bytes)
        .map_err(|err| BackendError::Internal(format!("decode json object failed: {err}")))?;
    Ok(Some(value))
}

fn temp_json_path(prefix: &str, start: Option<u64>, end: Option<u64>) -> PathBuf {
    let suffix = match (start, end) {
        (Some(start), Some(end)) => format!("-{start}-{end}"),
        _ => String::new(),
    };
    std::env::temp_dir().join(format!(
        "{prefix}-{}{}-{}.json",
        std::process::id(),
        suffix,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::local::LocalBackend;
    use std::io::Write;

    #[test]
    fn test_archive_and_download() {
        let temp_dir = std::env::temp_dir().join("reddb_archiver_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let backend_dir = temp_dir.join("backend");
        let _ = std::fs::create_dir_all(&backend_dir);

        let backend = Arc::new(LocalBackend);
        let archiver = WalArchiver::new(backend, "wal/");

        // Create a fake WAL file
        let wal_path = temp_dir.join("test.wal");
        {
            let mut f = std::fs::File::create(&wal_path).unwrap();
            f.write_all(b"fake wal data").unwrap();
        }

        // Archive it
        let meta = archiver.archive_segment(&wal_path, 8, 500).unwrap();
        assert_eq!(meta.lsn_start, 8);
        assert_eq!(meta.lsn_end, 500);
        assert!(meta.key.starts_with("wal/"));
        assert!(meta.key.ends_with(".wal"));

        // Download it
        let dest = temp_dir.join("downloaded.wal");
        let found = archiver.download_segment(&meta.key, &dest).unwrap();
        assert!(found);
        assert!(dest.exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_backup_head_roundtrip() {
        let temp_dir = std::env::temp_dir().join("reddb_backup_head_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let backend = LocalBackend;
        let head_key = temp_dir.join("manifests").join("head.json");

        let head = BackupHead {
            timeline_id: "main".to_string(),
            snapshot_key: "snapshots/000001-123.snapshot".to_string(),
            snapshot_id: 1,
            snapshot_time: 123,
            current_lsn: 456,
            last_archived_lsn: 456,
            wal_prefix: "wal/".to_string(),
        };

        publish_backup_head(&backend, &head_key.to_string_lossy(), &head).unwrap();
        let loaded = load_backup_head(&backend, &head_key.to_string_lossy())
            .unwrap()
            .expect("head");
        assert_eq!(loaded, head);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_snapshot_manifest_roundtrip() {
        let temp_dir = std::env::temp_dir().join("reddb_snapshot_manifest_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let backend = LocalBackend;
        let manifest = SnapshotManifest {
            timeline_id: "main".to_string(),
            snapshot_key: temp_dir
                .join("snapshots")
                .join("000001-123.snapshot")
                .to_string_lossy()
                .to_string(),
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

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_archived_change_records_roundtrip() {
        let temp_dir = std::env::temp_dir().join("reddb_archived_change_records_test");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let backend = LocalBackend;
        let prefix = format!("{}/wal/", temp_dir.to_string_lossy());
        let record = ChangeRecord {
            lsn: 7,
            timestamp: 1234,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 42,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: None,
        };

        let meta = archive_change_records(&backend, &prefix, &[(record.lsn, record.encode())])
            .unwrap()
            .expect("meta");
        let loaded = load_archived_change_records(&backend, &meta.key).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].lsn, 7);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
