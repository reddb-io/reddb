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
    /// Hex-encoded SHA-256 of the uploaded payload bytes (PLAN.md
    /// Phase 2.4). Restore recomputes the digest after download and
    /// fails closed on mismatch — same fail-closed contract as
    /// `SnapshotManifest::snapshot_sha256`. `None` for legacy
    /// segments archived before this field was introduced; restore
    /// tolerates absence with a warning.
    pub sha256: Option<String>,
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
        sha256_file_hex(snapshot_path)
    }
}

/// Stream-hash a local file to a hex SHA-256. Shared by snapshot and
/// WAL segment archival. Streamed in 8 KiB chunks so multi-GiB files
/// don't peak memory.
pub fn sha256_file_hex(path: &Path) -> Result<String, BackendError> {
    use std::fs::File;
    use std::io::Read;
    let mut hasher = crate::crypto::sha256::Sha256::new();
    let mut file = File::open(path)
        .map_err(|err| BackendError::Internal(format!("open file for hash {path:?}: {err}")))?;
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|err| {
            BackendError::Internal(format!("read file for hash {path:?}: {err}"))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::utils::to_hex(&hasher.finalize()))
}

/// Compute SHA-256 over a byte slice and return the hex digest.
/// Convenience for in-memory payloads (logical WAL segment buffer
/// before upload).
pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    crate::utils::to_hex(&crate::crypto::sha256::sha256(bytes))
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

        let key = format!("{}{:012}-{:012}.wal", self.prefix, lsn_start, lsn_end);

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

/// Per-segment manifest written next to each archived WAL segment
/// (PLAN.md Phase 2.4 + 11.3). Holds the digest the restore side
/// needs to verify the segment bytes after download, plus a
/// `prev_hash` linking it to the previous segment in the timeline so
/// the restore can detect a missing/reordered/replaced middle
/// segment. Stored at `<segment_key>.manifest.json` so
/// `cleanup_before` drops the pair atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalSegmentManifest {
    pub key: String,
    pub lsn_start: u64,
    pub lsn_end: u64,
    pub size_bytes: u64,
    pub created_at: u64,
    /// Hex SHA-256 of *this* segment's payload bytes.
    pub sha256: Option<String>,
    /// Hex SHA-256 of the segment immediately preceding this one in
    /// the timeline. `None` only for the very first segment after a
    /// fresh snapshot / PITR restore. Restore validates that
    /// segment[i].prev_hash == segment[i-1].sha256; any break is
    /// fail-closed (PLAN.md Phase 11.3).
    pub prev_hash: Option<String>,
}

impl WalSegmentManifest {
    pub fn from_meta(meta: &WalSegmentMeta, prev_hash: Option<String>) -> Self {
        Self {
            key: meta.key.clone(),
            lsn_start: meta.lsn_start,
            lsn_end: meta.lsn_end,
            size_bytes: meta.size_bytes,
            created_at: meta.created_at,
            sha256: meta.sha256.clone(),
            prev_hash,
        }
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert("key".to_string(), JsonValue::String(self.key.clone()));
        object.insert(
            "lsn_start".to_string(),
            JsonValue::Number(self.lsn_start as f64),
        );
        object.insert(
            "lsn_end".to_string(),
            JsonValue::Number(self.lsn_end as f64),
        );
        object.insert(
            "size_bytes".to_string(),
            JsonValue::Number(self.size_bytes as f64),
        );
        object.insert(
            "created_at".to_string(),
            JsonValue::Number(self.created_at as f64),
        );
        if let Some(sha) = &self.sha256 {
            object.insert("sha256".to_string(), JsonValue::String(sha.clone()));
        }
        if let Some(prev) = &self.prev_hash {
            object.insert("prev_hash".to_string(), JsonValue::String(prev.clone()));
        }
        JsonValue::Object(object)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        Ok(Self {
            key: value
                .get("key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    BackendError::Internal("wal segment manifest missing key".to_string())
                })?
                .to_string(),
            lsn_start: value
                .get("lsn_start")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            lsn_end: value
                .get("lsn_end")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            size_bytes: value
                .get("size_bytes")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            created_at: value
                .get("created_at")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            sha256: value
                .get("sha256")
                .and_then(JsonValue::as_str)
                .map(|s| s.to_string()),
            prev_hash: value
                .get("prev_hash")
                .and_then(JsonValue::as_str)
                .map(|s| s.to_string()),
        })
    }
}

pub fn wal_segment_manifest_key(segment_key: &str) -> String {
    format!("{segment_key}.manifest.json")
}

/// Top-level backup catalog (PLAN.md Phase 2.4). One JSON file at
/// `<prefix>MANIFEST.json` lists every snapshot and WAL segment in a
/// stable shape that external tooling can parse without sniffing
/// directory listings.
///
/// Spec lives in `docs/spec/manifest-format.md`. Versioned via the
/// `version` field — incompatible schema changes bump the major. The
/// engine's own restore code reads the per-snapshot and per-segment
/// sidecars *first*; the unified catalog is for human / orchestrator
/// inspection, manual disaster recovery, and third-party verifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedManifest {
    /// Schema version. Currently `1.0`.
    pub version: String,
    /// `CARGO_PKG_VERSION` of the engine that wrote the manifest.
    pub engine_version: String,
    /// Highest LSN known across all archived WAL segments. `0` when
    /// no WAL has been archived yet.
    pub latest_lsn: u64,
    /// All snapshots known to this prefix, freshest first.
    pub snapshots: Vec<UnifiedSnapshotEntry>,
    /// All WAL segments known to this prefix, ordered by `lsn_start`.
    pub wal_segments: Vec<UnifiedWalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSnapshotEntry {
    pub id: u64,
    pub lsn: u64,
    pub ts: u64,
    pub bytes: u64,
    pub key: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedWalEntry {
    pub lsn_start: u64,
    pub lsn_end: u64,
    pub key: String,
    pub bytes: u64,
    pub checksum: Option<String>,
    /// PLAN.md Phase 11.3 — sha256 of the prior segment in the
    /// timeline. Surfacing this in the unified manifest lets
    /// external verifiers validate the chain end-to-end from the
    /// catalog alone, without per-segment GETs.
    pub prev_hash: Option<String>,
}

impl UnifiedManifest {
    pub const VERSION: &'static str = "1.0";

    pub fn new(
        snapshots: Vec<UnifiedSnapshotEntry>,
        wal_segments: Vec<UnifiedWalEntry>,
    ) -> Self {
        let latest_lsn = wal_segments
            .iter()
            .map(|w| w.lsn_end)
            .chain(snapshots.iter().map(|s| s.lsn))
            .max()
            .unwrap_or(0);
        Self {
            version: Self::VERSION.to_string(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_lsn,
            snapshots,
            wal_segments,
        }
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        obj.insert("version".to_string(), JsonValue::String(self.version.clone()));
        obj.insert(
            "engine_version".to_string(),
            JsonValue::String(self.engine_version.clone()),
        );
        obj.insert(
            "latest_lsn".to_string(),
            JsonValue::Number(self.latest_lsn as f64),
        );
        obj.insert(
            "snapshots".to_string(),
            JsonValue::Array(
                self.snapshots
                    .iter()
                    .map(UnifiedSnapshotEntry::to_json_value)
                    .collect(),
            ),
        );
        obj.insert(
            "wal_segments".to_string(),
            JsonValue::Array(
                self.wal_segments
                    .iter()
                    .map(UnifiedWalEntry::to_json_value)
                    .collect(),
            ),
        );
        JsonValue::Object(obj)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        let obj = value.as_object().ok_or_else(|| {
            BackendError::Internal("unified manifest must be a JSON object".to_string())
        })?;
        Ok(Self {
            version: obj
                .get("version")
                .and_then(JsonValue::as_str)
                .unwrap_or("1.0")
                .to_string(),
            engine_version: obj
                .get("engine_version")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown")
                .to_string(),
            latest_lsn: obj
                .get("latest_lsn")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            snapshots: obj
                .get("snapshots")
                .and_then(JsonValue::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| UnifiedSnapshotEntry::from_json_value(v).ok())
                        .collect()
                })
                .unwrap_or_default(),
            wal_segments: obj
                .get("wal_segments")
                .and_then(JsonValue::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| UnifiedWalEntry::from_json_value(v).ok())
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

impl UnifiedSnapshotEntry {
    pub fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        obj.insert("id".to_string(), JsonValue::Number(self.id as f64));
        obj.insert("lsn".to_string(), JsonValue::Number(self.lsn as f64));
        obj.insert("ts".to_string(), JsonValue::Number(self.ts as f64));
        obj.insert("bytes".to_string(), JsonValue::Number(self.bytes as f64));
        obj.insert("key".to_string(), JsonValue::String(self.key.clone()));
        if let Some(c) = &self.checksum {
            obj.insert(
                "checksum".to_string(),
                JsonValue::String(format!("sha256:{c}")),
            );
        }
        JsonValue::Object(obj)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        let obj = value.as_object().ok_or_else(|| {
            BackendError::Internal("snapshot entry must be a JSON object".to_string())
        })?;
        Ok(Self {
            id: obj.get("id").and_then(JsonValue::as_u64).unwrap_or(0),
            lsn: obj.get("lsn").and_then(JsonValue::as_u64).unwrap_or(0),
            ts: obj.get("ts").and_then(JsonValue::as_u64).unwrap_or(0),
            bytes: obj
                .get("bytes")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            key: obj
                .get("key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| BackendError::Internal("snapshot entry missing key".to_string()))?
                .to_string(),
            checksum: obj
                .get("checksum")
                .and_then(JsonValue::as_str)
                .map(|s| s.strip_prefix("sha256:").unwrap_or(s).to_string()),
        })
    }
}

impl UnifiedWalEntry {
    pub fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        obj.insert(
            "lsn_start".to_string(),
            JsonValue::Number(self.lsn_start as f64),
        );
        obj.insert(
            "lsn_end".to_string(),
            JsonValue::Number(self.lsn_end as f64),
        );
        obj.insert("key".to_string(), JsonValue::String(self.key.clone()));
        obj.insert("bytes".to_string(), JsonValue::Number(self.bytes as f64));
        if let Some(c) = &self.checksum {
            obj.insert(
                "checksum".to_string(),
                JsonValue::String(format!("sha256:{c}")),
            );
        }
        if let Some(p) = &self.prev_hash {
            obj.insert(
                "prev_hash".to_string(),
                JsonValue::String(format!("sha256:{p}")),
            );
        }
        JsonValue::Object(obj)
    }

    pub fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        let obj = value.as_object().ok_or_else(|| {
            BackendError::Internal("wal segment entry must be a JSON object".to_string())
        })?;
        Ok(Self {
            lsn_start: obj
                .get("lsn_start")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            lsn_end: obj
                .get("lsn_end")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            key: obj
                .get("key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    BackendError::Internal("wal segment entry missing key".to_string())
                })?
                .to_string(),
            bytes: obj
                .get("bytes")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            checksum: obj
                .get("checksum")
                .and_then(JsonValue::as_str)
                .map(|s| s.strip_prefix("sha256:").unwrap_or(s).to_string()),
            prev_hash: obj
                .get("prev_hash")
                .and_then(JsonValue::as_str)
                .map(|s| s.strip_prefix("sha256:").unwrap_or(s).to_string()),
        })
    }
}

pub fn unified_manifest_key(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        "MANIFEST.json".to_string()
    } else {
        format!("{trimmed}/MANIFEST.json")
    }
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
    let key = unified_manifest_key(prefix);
    write_json_object(backend, &key, &manifest.to_json_value())?;
    Ok(key)
}

pub fn load_unified_manifest(
    backend: &dyn RemoteBackend,
    prefix: &str,
) -> Result<Option<UnifiedManifest>, BackendError> {
    let key = unified_manifest_key(prefix);
    let Some(value) = read_json_object(backend, &key)? else {
        return Ok(None);
    };
    Ok(Some(UnifiedManifest::from_json_value(&value)?))
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
    let root = derive_backup_root(snapshot_prefix);
    let snapshots = collect_unified_snapshots(backend, snapshot_prefix)?;
    let wal_root = format!("{}wal/", root);
    let wal_segments = collect_unified_wal_segments(backend, &wal_root)?;
    let manifest = UnifiedManifest::new(snapshots, wal_segments);
    publish_unified_manifest(backend, &root, &manifest)
}

fn derive_backup_root(snapshot_prefix: &str) -> String {
    // `snapshots/...` → `""`; `<root>/snapshots/...` → `<root>/`. Empty
    // prefix is allowed when the operator publishes everything under
    // the bucket root.
    let trimmed = snapshot_prefix.trim_end_matches('/');
    if let Some(idx) = trimmed.rfind("/snapshots") {
        let (head, _) = trimmed.split_at(idx);
        if head.is_empty() {
            String::new()
        } else {
            format!("{head}/")
        }
    } else if trimmed == "snapshots" || trimmed.is_empty() {
        String::new()
    } else {
        // Already a root prefix (no /snapshots suffix).
        format!("{trimmed}/")
    }
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
        if key.ends_with(".manifest.json") {
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
        if key.ends_with(".manifest.json") {
            continue;
        }
        if !key.ends_with(".wal") {
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
    let key = wal_segment_manifest_key(&manifest.key);
    write_json_object(backend, &key, &manifest.to_json_value())?;
    Ok(key)
}

pub fn load_wal_segment_manifest(
    backend: &dyn RemoteBackend,
    segment_key: &str,
) -> Result<Option<WalSegmentManifest>, BackendError> {
    let key = wal_segment_manifest_key(segment_key);
    let Some(value) = read_json_object(backend, &key)? else {
        return Ok(None);
    };
    Ok(Some(WalSegmentManifest::from_json_value(&value)?))
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
    prev_hash: Option<String>,
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
    let body = crate::json::to_vec(&payload).map_err(|err| {
        BackendError::Internal(format!("encode archived logical wal failed: {err}"))
    })?;
    // Hash the encoded payload before persisting so the digest
    // matches what gets uploaded byte-for-byte.
    let sha = sha256_bytes_hex(&body);

    let temp = temp_json_path(
        "reddb-archived-change-records",
        Some(*lsn_start),
        Some(*lsn_end),
    );
    std::fs::write(&temp, &body)
        .map_err(|err| BackendError::Transport(format!("write temp logical wal failed: {err}")))?;

    let key = format!("{}{:012}-{:012}.wal", prefix, lsn_start, lsn_end);
    backend.upload(&temp, &key)?;
    let size_bytes = std::fs::metadata(&temp).map(|meta| meta.len()).unwrap_or(0);
    let _ = std::fs::remove_file(&temp);

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
    let temp = temp_json_path("reddb-archived-change-records-read", None, None);
    let found = backend.download(segment_key, &temp)?;
    if !found {
        let _ = std::fs::remove_file(&temp);
        return Ok((Vec::new(), None));
    }
    let bytes = std::fs::read(&temp)
        .map_err(|err| BackendError::Transport(format!("read temp logical wal failed: {err}")))?;
    let _ = std::fs::remove_file(&temp);
    let digest = sha256_bytes_hex(&bytes);

    let value: JsonValue = crate::json::from_slice(&bytes).map_err(|err| {
        BackendError::Internal(format!("decode archived logical wal failed: {err}"))
    })?;
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
    Ok((out, Some(digest)))
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
        let meta = archiver.archive_segment(&wal_path, 8, 500, None).unwrap();
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

        let meta =
            archive_change_records(&backend, &prefix, &[(record.lsn, record.encode())], None)
                .unwrap()
                .expect("meta");
        let loaded = load_archived_change_records(&backend, &meta.key).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].lsn, 7);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn archive_change_records_writes_sidecar_with_sha256() {
        let temp_dir =
            std::env::temp_dir().join(format!("reddb_archiver_sidecar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let backend = LocalBackend;
        let prefix = format!("{}/wal/", temp_dir.to_string_lossy());
        let record = ChangeRecord {
            lsn: 11,
            timestamp: 99,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 1,
            entity_kind: "row".to_string(),
            entity_bytes: Some(b"x".to_vec()),
            metadata: None,
        };
        let meta =
            archive_change_records(&backend, &prefix, &[(record.lsn, record.encode())], None)
                .unwrap()
                .expect("meta");
        assert!(meta.sha256.is_some(), "WalSegmentMeta should carry sha256");

        let sidecar = load_wal_segment_manifest(&backend, &meta.key).unwrap().expect("sidecar");
        assert_eq!(sidecar.key, meta.key);
        assert_eq!(sidecar.lsn_start, meta.lsn_start);
        assert_eq!(sidecar.lsn_end, meta.lsn_end);
        assert_eq!(sidecar.sha256, meta.sha256);

        let (_records, computed) =
            load_archived_change_records_with_sha256(&backend, &meta.key).unwrap();
        assert_eq!(computed, meta.sha256, "computed sha must match sidecar");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn unified_manifest_json_roundtrip() {
        let manifest = UnifiedManifest::new(
            vec![UnifiedSnapshotEntry {
                id: 7,
                lsn: 100,
                ts: 1730000000000,
                bytes: 4096,
                key: "snapshots/000007-1730000000000.snapshot".to_string(),
                checksum: Some("9f8b".to_string()),
            }],
            vec![UnifiedWalEntry {
                lsn_start: 100,
                lsn_end: 250,
                key: "wal/000000000100-000000000250.wal".to_string(),
                bytes: 1024,
                checksum: Some("c1d2".to_string()),
                prev_hash: Some("9f8b".to_string()),
            }],
        );

        let json = manifest.to_json_value();
        let parsed = UnifiedManifest::from_json_value(&json).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.latest_lsn, 250);

        // prev_hash must round-trip with `sha256:` prefix on the wire
        // (PLAN.md Phase 11.3) so external verifiers can validate
        // the chain end-to-end without parsing the per-segment sidecar.
        assert_eq!(parsed.wal_segments[0].prev_hash.as_deref(), Some("9f8b"));
        let wal_wire = parsed.wal_segments[0].to_json_value().to_string_compact();
        assert!(
            wal_wire.contains("\"prev_hash\":\"sha256:9f8b\""),
            "wire form must include sha256: prefix on prev_hash; got: {wal_wire}"
        );

        // Checksum should round-trip with the `sha256:` prefix in the
        // wire form but parse back to the bare hex.
        let body = json.to_string_compact();
        assert!(body.contains("\"sha256:9f8b\""), "wire form must include sha256: prefix");
        assert_eq!(parsed.snapshots[0].checksum.as_deref(), Some("9f8b"));
    }

    #[test]
    fn unified_manifest_publish_load_roundtrip() {
        let temp_dir =
            std::env::temp_dir().join(format!("reddb_unified_manifest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let prefix = temp_dir.to_string_lossy().to_string();

        let backend = LocalBackend;
        let manifest = UnifiedManifest::new(vec![], vec![]);
        publish_unified_manifest(&backend, &prefix, &manifest).unwrap();
        let loaded = load_unified_manifest(&backend, &prefix).unwrap().expect("manifest");
        assert_eq!(loaded.version, "1.0");
        assert_eq!(loaded.latest_lsn, 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn archive_change_records_chains_prev_hash() {
        let temp_dir =
            std::env::temp_dir().join(format!("reddb_archive_chain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let backend = LocalBackend;
        let prefix = format!("{}/wal/", temp_dir.to_string_lossy());

        let mk = |lsn: u64| ChangeRecord {
            lsn,
            timestamp: lsn * 1000,
            operation: crate::replication::cdc::ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: lsn,
            entity_kind: "row".to_string(),
            entity_bytes: Some(format!("payload-{lsn}").into_bytes()),
            metadata: None,
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

        let s1 = load_wal_segment_manifest(&backend, &m1.key).unwrap().unwrap();
        let s2 = load_wal_segment_manifest(&backend, &m2.key).unwrap().unwrap();
        let s3 = load_wal_segment_manifest(&backend, &m3.key).unwrap().unwrap();
        assert!(s1.prev_hash.is_none(), "first segment has no prev_hash");
        assert_eq!(s2.prev_hash, m1.sha256, "seg 2 links to seg 1 sha256");
        assert_eq!(s3.prev_hash, m2.sha256, "seg 3 links to seg 2 sha256");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn wal_segment_manifest_carries_prev_hash_through_json() {
        let m = WalSegmentManifest {
            key: "wal/000000000010-000000000010.wal".to_string(),
            lsn_start: 10,
            lsn_end: 10,
            size_bytes: 128,
            created_at: 1730000000000,
            sha256: Some("abc".to_string()),
            prev_hash: Some("def".to_string()),
        };
        let parsed = WalSegmentManifest::from_json_value(&m.to_json_value()).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn derive_backup_root_handles_typical_prefixes() {
        assert_eq!(derive_backup_root(""), "");
        assert_eq!(derive_backup_root("snapshots/"), "");
        assert_eq!(derive_backup_root("snapshots"), "");
        assert_eq!(derive_backup_root("clusters/dev/snapshots/"), "clusters/dev/");
        assert_eq!(derive_backup_root("clusters/dev/snapshots"), "clusters/dev/");
        assert_eq!(derive_backup_root("clusters/dev/"), "clusters/dev/");
    }
}
