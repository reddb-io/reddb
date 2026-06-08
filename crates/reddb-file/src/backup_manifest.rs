//! Backup, snapshot, and archived WAL manifest contracts.
//!
//! `reddb-server` owns when artifacts are uploaded or restored. This module
//! owns the persisted manifest structs, JSON field names, sidecar key names,
//! checksum formatting, and snapshot hash computation.

use serde_json::{Map, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

pub const BACKUP_MANIFEST_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalSegmentMeta {
    pub key: String,
    pub lsn_start: u64,
    pub lsn_end: u64,
    pub created_at: u64,
    pub size_bytes: u64,
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
    pub snapshot_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalSegmentManifest {
    pub key: String,
    pub lsn_start: u64,
    pub lsn_end: u64,
    pub size_bytes: u64,
    pub created_at: u64,
    pub sha256: Option<String>,
    pub prev_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedLogicalWalRecord {
    pub lsn: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedManifest {
    pub version: String,
    pub engine_version: String,
    pub latest_lsn: u64,
    pub snapshots: Vec<UnifiedSnapshotEntry>,
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
    pub prev_hash: Option<String>,
}

impl SnapshotManifest {
    pub fn compute_snapshot_sha256(snapshot_path: &Path) -> io::Result<String> {
        sha256_file_hex(snapshot_path)
    }
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
}

impl UnifiedManifest {
    pub const VERSION: &'static str = "1.0";

    pub fn new(snapshots: Vec<UnifiedSnapshotEntry>, wal_segments: Vec<UnifiedWalEntry>) -> Self {
        Self::new_with_engine_version(env!("CARGO_PKG_VERSION"), snapshots, wal_segments)
    }

    pub fn new_with_engine_version(
        engine_version: impl Into<String>,
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
            engine_version: engine_version.into(),
            latest_lsn,
            snapshots,
            wal_segments,
        }
    }
}

pub fn snapshot_manifest_key(snapshot_key: &str) -> String {
    format!("{snapshot_key}.manifest.json")
}

pub fn wal_segment_manifest_key(segment_key: &str) -> String {
    format!("{segment_key}.manifest.json")
}

pub fn is_backup_manifest_sidecar_key(key: &str) -> bool {
    key.ends_with(".manifest.json")
}

pub fn unified_manifest_key(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        "MANIFEST.json".to_string()
    } else {
        format!("{trimmed}/MANIFEST.json")
    }
}

pub fn backup_head_key(root_prefix: &str) -> String {
    format!(
        "{}manifests/head.json",
        normalize_backup_root_prefix(root_prefix)
    )
}

pub fn backup_snapshot_prefix(root_prefix: &str) -> String {
    format!("{}snapshots/", normalize_backup_root_prefix(root_prefix))
}

pub fn backup_wal_prefix(root_prefix: &str) -> String {
    format!("{}wal/", normalize_backup_root_prefix(root_prefix))
}

pub fn remote_database_key(root_prefix: &str) -> String {
    let trimmed = root_prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        "data.rdb".to_string()
    } else {
        format!("{trimmed}/data.rdb")
    }
}

pub fn backup_root_from_snapshot_prefix(snapshot_prefix: &str) -> String {
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
        normalize_backup_root_prefix(trimmed)
    }
}

pub fn archived_snapshot_key(prefix: &str, snapshot_id: u64, timestamp_ms: u64) -> String {
    format!("{prefix}{snapshot_id:012}-{timestamp_ms}.snapshot")
}

pub fn archived_wal_segment_key(prefix: &str, lsn_start: u64, lsn_end: u64) -> String {
    format!("{prefix}{lsn_start:012}-{lsn_end:012}.wal")
}

pub fn parse_archived_wal_segment_key(key: &str) -> Option<(u64, u64)> {
    let path = PathBuf::from(key);
    let file_name = path.file_name()?.to_str()?;
    let (start, end) = file_name.strip_suffix(".wal")?.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

pub fn is_archived_wal_segment_key(key: &str) -> bool {
    parse_archived_wal_segment_key(key).is_some()
}

fn normalize_backup_root_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

pub fn encode_backup_head_json(head: &BackupHead) -> io::Result<Vec<u8>> {
    encode_value(&backup_head_to_json(head))
}

pub fn decode_backup_head_json(bytes: &[u8]) -> io::Result<BackupHead> {
    let value = decode_value(bytes)?;
    backup_head_from_json(&value)
}

pub fn encode_snapshot_manifest_json(manifest: &SnapshotManifest) -> io::Result<Vec<u8>> {
    encode_value(&snapshot_manifest_to_json(manifest))
}

pub fn decode_snapshot_manifest_json(bytes: &[u8]) -> io::Result<SnapshotManifest> {
    let value = decode_value(bytes)?;
    snapshot_manifest_from_json(&value)
}

pub fn encode_wal_segment_manifest_json(manifest: &WalSegmentManifest) -> io::Result<Vec<u8>> {
    encode_value(&wal_segment_manifest_to_json(manifest))
}

pub fn decode_wal_segment_manifest_json(bytes: &[u8]) -> io::Result<WalSegmentManifest> {
    let value = decode_value(bytes)?;
    wal_segment_manifest_from_json(&value)
}

pub fn encode_archived_logical_wal_records(records: &[(u64, Vec<u8>)]) -> io::Result<Vec<u8>> {
    let values = records
        .iter()
        .map(|(lsn, data)| {
            let mut object = Map::new();
            object.insert("lsn".into(), u64_json(*lsn));
            object.insert("data".into(), JsonValue::String(to_hex(data)));
            JsonValue::Object(object)
        })
        .collect();
    encode_value(&JsonValue::Array(values))
}

pub fn decode_archived_logical_wal_records(
    bytes: &[u8],
) -> io::Result<Vec<ArchivedLogicalWalRecord>> {
    let value = decode_value(bytes)?;
    let Some(entries) = value.as_array() else {
        return Err(invalid_data("archived logical wal must be a JSON array"));
    };

    let mut out = Vec::new();
    for entry in entries {
        let Some(data_hex) = entry.get("data").and_then(JsonValue::as_str) else {
            continue;
        };
        out.push(ArchivedLogicalWalRecord {
            lsn: entry.get("lsn").and_then(JsonValue::as_u64).unwrap_or(0),
            data: from_hex(data_hex).map_err(|err| invalid_data(format!("decode hex: {err}")))?,
        });
    }
    Ok(out)
}

pub fn encode_unified_manifest_json(manifest: &UnifiedManifest) -> io::Result<Vec<u8>> {
    encode_value(&unified_manifest_to_json(manifest))
}

pub fn decode_unified_manifest_json(bytes: &[u8]) -> io::Result<UnifiedManifest> {
    let value = decode_value(bytes)?;
    unified_manifest_from_json(&value)
}

pub fn sha256_file_hex(path: &Path) -> io::Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

fn backup_head_to_json(head: &BackupHead) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "timeline_id".into(),
        JsonValue::String(head.timeline_id.clone()),
    );
    object.insert(
        "snapshot_key".into(),
        JsonValue::String(head.snapshot_key.clone()),
    );
    object.insert("snapshot_id".into(), u64_json(head.snapshot_id));
    object.insert("snapshot_time".into(), u64_json(head.snapshot_time));
    object.insert("current_lsn".into(), u64_json(head.current_lsn));
    object.insert("last_archived_lsn".into(), u64_json(head.last_archived_lsn));
    object.insert(
        "wal_prefix".into(),
        JsonValue::String(head.wal_prefix.clone()),
    );
    JsonValue::Object(object)
}

fn backup_head_from_json(value: &JsonValue) -> io::Result<BackupHead> {
    Ok(BackupHead {
        timeline_id: string_field(value, "timeline_id").unwrap_or_else(|| "main".to_string()),
        snapshot_key: required_string(value, "snapshot_key", "backup head missing snapshot_key")?,
        snapshot_id: required_u64(value, "snapshot_id", "backup head missing snapshot_id")?,
        snapshot_time: required_u64(value, "snapshot_time", "backup head missing snapshot_time")?,
        current_lsn: u64_field(value, "current_lsn").unwrap_or(0),
        last_archived_lsn: u64_field(value, "last_archived_lsn").unwrap_or(0),
        wal_prefix: string_field(value, "wal_prefix").unwrap_or_else(|| "wal/".to_string()),
    })
}

fn snapshot_manifest_to_json(manifest: &SnapshotManifest) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "timeline_id".into(),
        JsonValue::String(manifest.timeline_id.clone()),
    );
    object.insert(
        "snapshot_key".into(),
        JsonValue::String(manifest.snapshot_key.clone()),
    );
    object.insert("snapshot_id".into(), u64_json(manifest.snapshot_id));
    object.insert("snapshot_time".into(), u64_json(manifest.snapshot_time));
    object.insert("base_lsn".into(), u64_json(manifest.base_lsn));
    object.insert(
        "schema_version".into(),
        u64_json(manifest.schema_version as u64),
    );
    object.insert(
        "format_version".into(),
        u64_json(manifest.format_version as u64),
    );
    if let Some(sha) = &manifest.snapshot_sha256 {
        object.insert("snapshot_sha256".into(), JsonValue::String(sha.clone()));
    }
    JsonValue::Object(object)
}

fn snapshot_manifest_from_json(value: &JsonValue) -> io::Result<SnapshotManifest> {
    Ok(SnapshotManifest {
        timeline_id: string_field(value, "timeline_id").unwrap_or_else(|| "main".to_string()),
        snapshot_key: required_string(
            value,
            "snapshot_key",
            "snapshot manifest missing snapshot_key",
        )?,
        snapshot_id: required_u64(
            value,
            "snapshot_id",
            "snapshot manifest missing snapshot_id",
        )?,
        snapshot_time: required_u64(
            value,
            "snapshot_time",
            "snapshot manifest missing snapshot_time",
        )?,
        base_lsn: u64_field(value, "base_lsn").unwrap_or(0),
        schema_version: u64_field(value, "schema_version")
            .unwrap_or(BACKUP_MANIFEST_FORMAT_VERSION as u64) as u32,
        format_version: u64_field(value, "format_version")
            .unwrap_or(BACKUP_MANIFEST_FORMAT_VERSION as u64) as u32,
        snapshot_sha256: string_field(value, "snapshot_sha256"),
    })
}

fn wal_segment_manifest_to_json(manifest: &WalSegmentManifest) -> JsonValue {
    let mut object = Map::new();
    object.insert("key".into(), JsonValue::String(manifest.key.clone()));
    object.insert("lsn_start".into(), u64_json(manifest.lsn_start));
    object.insert("lsn_end".into(), u64_json(manifest.lsn_end));
    object.insert("size_bytes".into(), u64_json(manifest.size_bytes));
    object.insert("created_at".into(), u64_json(manifest.created_at));
    if let Some(sha) = &manifest.sha256 {
        object.insert("sha256".into(), JsonValue::String(sha.clone()));
    }
    if let Some(prev) = &manifest.prev_hash {
        object.insert("prev_hash".into(), JsonValue::String(prev.clone()));
    }
    JsonValue::Object(object)
}

fn wal_segment_manifest_from_json(value: &JsonValue) -> io::Result<WalSegmentManifest> {
    Ok(WalSegmentManifest {
        key: required_string(value, "key", "wal segment manifest missing key")?,
        lsn_start: u64_field(value, "lsn_start").unwrap_or(0),
        lsn_end: u64_field(value, "lsn_end").unwrap_or(0),
        size_bytes: u64_field(value, "size_bytes").unwrap_or(0),
        created_at: u64_field(value, "created_at").unwrap_or(0),
        sha256: string_field(value, "sha256"),
        prev_hash: string_field(value, "prev_hash"),
    })
}

fn unified_manifest_to_json(manifest: &UnifiedManifest) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "version".into(),
        JsonValue::String(manifest.version.clone()),
    );
    object.insert(
        "engine_version".into(),
        JsonValue::String(manifest.engine_version.clone()),
    );
    object.insert("latest_lsn".into(), u64_json(manifest.latest_lsn));
    object.insert(
        "snapshots".into(),
        JsonValue::Array(
            manifest
                .snapshots
                .iter()
                .map(unified_snapshot_to_json)
                .collect(),
        ),
    );
    object.insert(
        "wal_segments".into(),
        JsonValue::Array(
            manifest
                .wal_segments
                .iter()
                .map(unified_wal_to_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn unified_manifest_from_json(value: &JsonValue) -> io::Result<UnifiedManifest> {
    let Some(object) = value.as_object() else {
        return Err(invalid_data("unified manifest must be a JSON object"));
    };
    let snapshots = object
        .get("snapshots")
        .and_then(JsonValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| unified_snapshot_from_json(entry).ok())
                .collect()
        })
        .unwrap_or_default();
    let wal_segments = object
        .get("wal_segments")
        .and_then(JsonValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| unified_wal_from_json(entry).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(UnifiedManifest {
        version: string_field(value, "version").unwrap_or_else(|| "1.0".to_string()),
        engine_version: string_field(value, "engine_version")
            .unwrap_or_else(|| "unknown".to_string()),
        latest_lsn: u64_field(value, "latest_lsn").unwrap_or(0),
        snapshots,
        wal_segments,
    })
}

fn unified_snapshot_to_json(entry: &UnifiedSnapshotEntry) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".into(), u64_json(entry.id));
    object.insert("lsn".into(), u64_json(entry.lsn));
    object.insert("ts".into(), u64_json(entry.ts));
    object.insert("bytes".into(), u64_json(entry.bytes));
    object.insert("key".into(), JsonValue::String(entry.key.clone()));
    if let Some(checksum) = &entry.checksum {
        object.insert(
            "checksum".into(),
            JsonValue::String(format!("sha256:{checksum}")),
        );
    }
    JsonValue::Object(object)
}

fn unified_snapshot_from_json(value: &JsonValue) -> io::Result<UnifiedSnapshotEntry> {
    let Some(object) = value.as_object() else {
        return Err(invalid_data("snapshot entry must be a JSON object"));
    };
    Ok(UnifiedSnapshotEntry {
        id: object.get("id").and_then(JsonValue::as_u64).unwrap_or(0),
        lsn: object.get("lsn").and_then(JsonValue::as_u64).unwrap_or(0),
        ts: object.get("ts").and_then(JsonValue::as_u64).unwrap_or(0),
        bytes: object.get("bytes").and_then(JsonValue::as_u64).unwrap_or(0),
        key: object
            .get("key")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| invalid_data("snapshot entry missing key"))?
            .to_string(),
        checksum: object
            .get("checksum")
            .and_then(JsonValue::as_str)
            .map(strip_sha256_prefix),
    })
}

fn unified_wal_to_json(entry: &UnifiedWalEntry) -> JsonValue {
    let mut object = Map::new();
    object.insert("lsn_start".into(), u64_json(entry.lsn_start));
    object.insert("lsn_end".into(), u64_json(entry.lsn_end));
    object.insert("key".into(), JsonValue::String(entry.key.clone()));
    object.insert("bytes".into(), u64_json(entry.bytes));
    if let Some(checksum) = &entry.checksum {
        object.insert(
            "checksum".into(),
            JsonValue::String(format!("sha256:{checksum}")),
        );
    }
    if let Some(prev_hash) = &entry.prev_hash {
        object.insert(
            "prev_hash".into(),
            JsonValue::String(format!("sha256:{prev_hash}")),
        );
    }
    JsonValue::Object(object)
}

fn unified_wal_from_json(value: &JsonValue) -> io::Result<UnifiedWalEntry> {
    let Some(object) = value.as_object() else {
        return Err(invalid_data("wal segment entry must be a JSON object"));
    };
    Ok(UnifiedWalEntry {
        lsn_start: object
            .get("lsn_start")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        lsn_end: object
            .get("lsn_end")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        key: object
            .get("key")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| invalid_data("wal segment entry missing key"))?
            .to_string(),
        bytes: object.get("bytes").and_then(JsonValue::as_u64).unwrap_or(0),
        checksum: object
            .get("checksum")
            .and_then(JsonValue::as_str)
            .map(strip_sha256_prefix),
        prev_hash: object
            .get("prev_hash")
            .and_then(JsonValue::as_str)
            .map(strip_sha256_prefix),
    })
}

fn encode_value(value: &JsonValue) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|err| invalid_data(format!("encode manifest json: {err}")))
}

fn decode_value(bytes: &[u8]) -> io::Result<JsonValue> {
    serde_json::from_slice(bytes)
        .map_err(|err| invalid_data(format!("decode manifest json: {err}")))
}

fn required_string(value: &JsonValue, field: &str, message: &'static str) -> io::Result<String> {
    string_field(value, field).ok_or_else(|| invalid_data(message))
}

fn required_u64(value: &JsonValue, field: &str, message: &'static str) -> io::Result<u64> {
    u64_field(value, field).ok_or_else(|| invalid_data(message))
}

fn string_field(value: &JsonValue, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn u64_field(value: &JsonValue, field: &str) -> Option<u64> {
    value.get(field).and_then(JsonValue::as_u64)
}

fn u64_json(value: u64) -> JsonValue {
    JsonValue::Number(value.into())
}

fn strip_sha256_prefix(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_string()
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn from_hex(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.len() % 2 != 0 {
        return Err("odd-length hex string");
    }

    let mut out = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for idx in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[idx]).ok_or("invalid hex digit")?;
        let low = hex_nibble(bytes[idx + 1]).ok_or("invalid hex digit")?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_manifest_json_round_trips() {
        let head = BackupHead {
            timeline_id: "main".into(),
            snapshot_key: "snapshots/1.snapshot".into(),
            snapshot_id: 1,
            snapshot_time: 2,
            current_lsn: 3,
            last_archived_lsn: 4,
            wal_prefix: "wal/".into(),
        };
        assert_eq!(
            decode_backup_head_json(&encode_backup_head_json(&head).unwrap()).unwrap(),
            head
        );

        let snapshot = SnapshotManifest {
            timeline_id: "main".into(),
            snapshot_key: "snapshots/1.snapshot".into(),
            snapshot_id: 1,
            snapshot_time: 2,
            base_lsn: 3,
            schema_version: BACKUP_MANIFEST_FORMAT_VERSION,
            format_version: BACKUP_MANIFEST_FORMAT_VERSION,
            snapshot_sha256: Some("abc".into()),
        };
        assert_eq!(
            decode_snapshot_manifest_json(&encode_snapshot_manifest_json(&snapshot).unwrap())
                .unwrap(),
            snapshot
        );
    }

    #[test]
    fn wal_segment_manifest_json_round_trips() {
        let manifest = WalSegmentManifest {
            key: "wal/000000000010-000000000020.wal".into(),
            lsn_start: 10,
            lsn_end: 20,
            size_bytes: 128,
            created_at: 30,
            sha256: Some("abc".into()),
            prev_hash: Some("def".into()),
        };
        assert_eq!(
            decode_wal_segment_manifest_json(&encode_wal_segment_manifest_json(&manifest).unwrap())
                .unwrap(),
            manifest
        );
        assert_eq!(
            wal_segment_manifest_key(&manifest.key),
            "wal/000000000010-000000000020.wal.manifest.json"
        );
        assert_eq!(
            archived_wal_segment_key("wal/", 10, 20),
            "wal/000000000010-000000000020.wal"
        );
        assert_eq!(
            parse_archived_wal_segment_key("wal/000000000010-000000000020.wal"),
            Some((10, 20))
        );
        assert!(is_archived_wal_segment_key(
            "wal/000000000010-000000000020.wal"
        ));
        assert!(is_backup_manifest_sidecar_key(
            "wal/000000000010-000000000020.wal.manifest.json"
        ));
        assert_eq!(
            parse_archived_wal_segment_key("wal/not-a-segment.wal"),
            None
        );
    }

    #[test]
    fn backup_artifact_keys_and_prefixes_are_canonical() {
        assert_eq!(
            backup_head_key("tenant/db/"),
            "tenant/db/manifests/head.json"
        );
        assert_eq!(
            backup_head_key("/tenant/db/"),
            "tenant/db/manifests/head.json"
        );
        assert_eq!(backup_head_key(""), "manifests/head.json");
        assert_eq!(backup_snapshot_prefix("tenant/db"), "tenant/db/snapshots/");
        assert_eq!(backup_wal_prefix("tenant/db"), "tenant/db/wal/");
        assert_eq!(remote_database_key("tenant/db/"), "tenant/db/data.rdb");
        assert_eq!(
            remote_database_key("/var/lib/reddb"),
            "/var/lib/reddb/data.rdb"
        );
        assert_eq!(remote_database_key(""), "data.rdb");
        assert_eq!(
            archived_snapshot_key("tenant/db/snapshots/", 7, 1730000000000),
            "tenant/db/snapshots/000000000007-1730000000000.snapshot"
        );
        assert_eq!(
            backup_root_from_snapshot_prefix("tenant/db/snapshots/"),
            "tenant/db/"
        );
        assert_eq!(
            backup_root_from_snapshot_prefix("tenant/db/snapshots/hourly/"),
            "tenant/db/"
        );
        assert_eq!(backup_root_from_snapshot_prefix("snapshots/"), "");
        assert_eq!(backup_root_from_snapshot_prefix("tenant/db"), "tenant/db/");
    }

    #[test]
    fn archived_logical_wal_records_json_round_trip() {
        let records = vec![(7, vec![0, 1, 2, 250, 255]), (9, b"wal".to_vec())];
        let body = encode_archived_logical_wal_records(&records).unwrap();
        let text = String::from_utf8(body.clone()).unwrap();
        assert!(text.contains("\"lsn\":7"));
        assert!(text.contains("\"data\":\"000102faff\""));

        let decoded = decode_archived_logical_wal_records(&body).unwrap();
        assert_eq!(
            decoded,
            vec![
                ArchivedLogicalWalRecord {
                    lsn: 7,
                    data: vec![0, 1, 2, 250, 255],
                },
                ArchivedLogicalWalRecord {
                    lsn: 9,
                    data: b"wal".to_vec(),
                },
            ]
        );
    }

    #[test]
    fn archived_logical_wal_records_decode_skips_entries_without_data() {
        let body = br#"[{"lsn":1},{"lsn":2,"data":"0a"}]"#;
        let decoded = decode_archived_logical_wal_records(body).unwrap();
        assert_eq!(
            decoded,
            vec![ArchivedLogicalWalRecord {
                lsn: 2,
                data: vec![10],
            }]
        );
    }

    #[test]
    fn unified_manifest_json_prefixes_checksums_on_wire() {
        let manifest = UnifiedManifest::new_with_engine_version(
            "server",
            vec![UnifiedSnapshotEntry {
                id: 7,
                lsn: 100,
                ts: 1730000000000,
                bytes: 4096,
                key: "snapshots/000007-1730000000000.snapshot".into(),
                checksum: Some("9f8b".into()),
            }],
            vec![UnifiedWalEntry {
                lsn_start: 100,
                lsn_end: 250,
                key: "wal/000000000100-000000000250.wal".into(),
                bytes: 1024,
                checksum: Some("c1d2".into()),
                prev_hash: Some("9f8b".into()),
            }],
        );

        let body = String::from_utf8(encode_unified_manifest_json(&manifest).unwrap()).unwrap();
        assert!(body.contains("\"checksum\":\"sha256:9f8b\""));
        assert!(body.contains("\"prev_hash\":\"sha256:9f8b\""));
        assert_eq!(
            decode_unified_manifest_json(body.as_bytes()).unwrap(),
            manifest
        );
    }
}
