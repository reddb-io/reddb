use serde_json::Value as JsonValue;

use super::util::{
    get_bool_default, get_opt_string, get_opt_u64, get_string, get_u64, hex_decode, hex_encode,
    object_from_slice, ReplicationPayloadError, Result,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBackupRequest {
    pub replica_id: Option<String>,
    pub max_bytes: Option<u64>,
    pub snapshot_offset: u64,
    pub snapshot_token: Option<String>,
}

impl BaseBackupRequest {
    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        if let Some(replica_id) = &self.replica_id {
            obj.insert(
                "replica_id".to_string(),
                JsonValue::String(replica_id.clone()),
            );
        }
        if let Some(max_bytes) = self.max_bytes {
            obj.insert("max_bytes".to_string(), JsonValue::Number(max_bytes.into()));
        }
        obj.insert(
            "snapshot_offset".to_string(),
            JsonValue::Number(self.snapshot_offset.into()),
        );
        if let Some(token) = &self.snapshot_token {
            obj.insert(
                "snapshot_token".to_string(),
                JsonValue::String(token.clone()),
            );
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            replica_id: get_opt_string(&obj, "replica_id"),
            max_bytes: get_opt_u64(&obj, "max_bytes"),
            snapshot_offset: get_opt_u64(&obj, "snapshot_offset").unwrap_or(0),
            snapshot_token: get_opt_string(&obj, "snapshot_token"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBackupManifestChunk {
    pub ordinal: u32,
    pub snapshot_offset: u64,
    pub bytes: u64,
    pub checksum: u64,
    pub relative_path: String,
}

impl BaseBackupManifestChunk {
    fn to_json(&self) -> JsonValue {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "ordinal".to_string(),
            JsonValue::Number(self.ordinal.into()),
        );
        obj.insert(
            "snapshot_offset".to_string(),
            JsonValue::Number(self.snapshot_offset.into()),
        );
        obj.insert("bytes".to_string(), JsonValue::Number(self.bytes.into()));
        obj.insert(
            "checksum".to_string(),
            JsonValue::Number(self.checksum.into()),
        );
        obj.insert(
            "relative_path".to_string(),
            JsonValue::String(self.relative_path.clone()),
        );
        JsonValue::Object(obj)
    }

    fn from_json(value: &JsonValue) -> Result<Self> {
        let obj = value
            .as_object()
            .ok_or(ReplicationPayloadError::InvalidField("basebackup_chunks"))?;
        let ordinal = get_u64(obj, "ordinal")?;
        Ok(Self {
            ordinal: u32::try_from(ordinal)
                .map_err(|_| ReplicationPayloadError::InvalidField("ordinal"))?,
            snapshot_offset: get_u64(obj, "snapshot_offset")?,
            bytes: get_u64(obj, "bytes")?,
            checksum: get_u64(obj, "checksum")?,
            relative_path: get_string(obj, "relative_path")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBackupChunk {
    pub snapshot_available: bool,
    pub replica_id: String,
    pub slot_restart_lsn: u64,
    pub snapshot_lsn: Option<u64>,
    pub snapshot_token: Option<String>,
    pub snapshot_total_bytes: Option<u64>,
    pub snapshot_offset: u64,
    pub next_snapshot_offset: Option<u64>,
    pub snapshot_complete: bool,
    pub snapshot_path: Option<String>,
    pub snapshot_chunk: Option<Vec<u8>>,
    pub snapshot_hex: Option<Vec<u8>>,
    pub metadata_binary: Option<Vec<u8>>,
    pub metadata_json: Option<Vec<u8>>,
    pub header_shadow: Option<Vec<u8>>,
    pub metadata_shadow: Option<Vec<u8>>,
    pub basebackup_available: bool,
    pub basebackup_timeline: Option<u64>,
    pub basebackup_start_lsn: Option<u64>,
    pub basebackup_checkpoint_lsn: Option<u64>,
    pub basebackup_snapshot_bytes: Option<u64>,
    pub basebackup_snapshot_checksum: Option<u64>,
    pub basebackup_manifest: Option<Vec<u8>>,
    pub basebackup_chunks: Vec<BaseBackupManifestChunk>,
    pub basebackup_chunk_ordinal: Option<u32>,
    pub basebackup_chunk: Option<Vec<u8>>,
}

impl BaseBackupChunk {
    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "snapshot_available".to_string(),
            JsonValue::Bool(self.snapshot_available),
        );
        obj.insert(
            "replica_id".to_string(),
            JsonValue::String(self.replica_id.clone()),
        );
        obj.insert(
            "slot_restart_lsn".to_string(),
            JsonValue::Number(self.slot_restart_lsn.into()),
        );
        if let Some(lsn) = self.snapshot_lsn {
            obj.insert("snapshot_lsn".to_string(), JsonValue::Number(lsn.into()));
        }
        if let Some(token) = &self.snapshot_token {
            obj.insert(
                "snapshot_token".to_string(),
                JsonValue::String(token.clone()),
            );
        }
        if let Some(bytes) = self.snapshot_total_bytes {
            obj.insert(
                "snapshot_total_bytes".to_string(),
                JsonValue::Number(bytes.into()),
            );
        }
        obj.insert(
            "snapshot_offset".to_string(),
            JsonValue::Number(self.snapshot_offset.into()),
        );
        if let Some(offset) = self.next_snapshot_offset {
            obj.insert(
                "next_snapshot_offset".to_string(),
                JsonValue::Number(offset.into()),
            );
        }
        obj.insert(
            "snapshot_complete".to_string(),
            JsonValue::Bool(self.snapshot_complete),
        );
        if let Some(path) = &self.snapshot_path {
            obj.insert("snapshot_path".to_string(), JsonValue::String(path.clone()));
        }
        if let Some(bytes) = &self.snapshot_chunk {
            obj.insert(
                "snapshot_chunk_hex".to_string(),
                JsonValue::String(hex_encode(bytes)),
            );
        }
        if let Some(bytes) = &self.snapshot_hex {
            obj.insert(
                "snapshot_hex".to_string(),
                JsonValue::String(hex_encode(bytes)),
            );
        }
        insert_opt_hex(&mut obj, "metadata_binary_hex", &self.metadata_binary);
        insert_opt_hex(&mut obj, "metadata_json_hex", &self.metadata_json);
        insert_opt_hex(&mut obj, "header_shadow_hex", &self.header_shadow);
        insert_opt_hex(&mut obj, "metadata_shadow_hex", &self.metadata_shadow);

        obj.insert(
            "basebackup_available".to_string(),
            JsonValue::Bool(self.basebackup_available),
        );
        insert_opt_u64(&mut obj, "basebackup_timeline", self.basebackup_timeline);
        insert_opt_u64(&mut obj, "basebackup_start_lsn", self.basebackup_start_lsn);
        insert_opt_u64(
            &mut obj,
            "basebackup_checkpoint_lsn",
            self.basebackup_checkpoint_lsn,
        );
        insert_opt_u64(
            &mut obj,
            "basebackup_snapshot_bytes",
            self.basebackup_snapshot_bytes,
        );
        insert_opt_u64(
            &mut obj,
            "basebackup_snapshot_checksum",
            self.basebackup_snapshot_checksum,
        );
        if let Some(bytes) = &self.basebackup_manifest {
            obj.insert(
                "basebackup_manifest_hex".to_string(),
                JsonValue::String(hex_encode(bytes)),
            );
        }
        obj.insert(
            "basebackup_chunks".to_string(),
            JsonValue::Array(
                self.basebackup_chunks
                    .iter()
                    .map(BaseBackupManifestChunk::to_json)
                    .collect(),
            ),
        );
        if let Some(ordinal) = self.basebackup_chunk_ordinal {
            obj.insert(
                "basebackup_chunk_ordinal".to_string(),
                JsonValue::Number(ordinal.into()),
            );
        }
        if let Some(bytes) = &self.basebackup_chunk {
            obj.insert(
                "basebackup_chunk_hex".to_string(),
                JsonValue::String(hex_encode(bytes)),
            );
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        let basebackup_chunks = match obj.get("basebackup_chunks") {
            Some(JsonValue::Array(values)) => values
                .iter()
                .map(BaseBackupManifestChunk::from_json)
                .collect::<Result<Vec<_>>>()?,
            Some(_) => return Err(ReplicationPayloadError::InvalidField("basebackup_chunks")),
            None => Vec::new(),
        };
        let basebackup_chunk_ordinal =
            match get_opt_u64(&obj, "basebackup_chunk_ordinal") {
                Some(value) => Some(u32::try_from(value).map_err(|_| {
                    ReplicationPayloadError::InvalidField("basebackup_chunk_ordinal")
                })?),
                None => None,
            };
        Ok(Self {
            snapshot_available: get_bool_default(&obj, "snapshot_available", false),
            replica_id: get_string(&obj, "replica_id")?,
            slot_restart_lsn: get_u64(&obj, "slot_restart_lsn")?,
            snapshot_lsn: get_opt_u64(&obj, "snapshot_lsn"),
            snapshot_token: get_opt_string(&obj, "snapshot_token"),
            snapshot_total_bytes: get_opt_u64(&obj, "snapshot_total_bytes"),
            snapshot_offset: get_opt_u64(&obj, "snapshot_offset").unwrap_or(0),
            next_snapshot_offset: get_opt_u64(&obj, "next_snapshot_offset"),
            snapshot_complete: get_bool_default(&obj, "snapshot_complete", false),
            snapshot_path: get_opt_string(&obj, "snapshot_path"),
            snapshot_chunk: decode_opt_hex(&obj, "snapshot_chunk_hex")?,
            snapshot_hex: decode_opt_hex(&obj, "snapshot_hex")?,
            metadata_binary: decode_opt_hex(&obj, "metadata_binary_hex")?,
            metadata_json: decode_opt_hex(&obj, "metadata_json_hex")?,
            header_shadow: decode_opt_hex(&obj, "header_shadow_hex")?,
            metadata_shadow: decode_opt_hex(&obj, "metadata_shadow_hex")?,
            basebackup_available: get_bool_default(&obj, "basebackup_available", false),
            basebackup_timeline: get_opt_u64(&obj, "basebackup_timeline"),
            basebackup_start_lsn: get_opt_u64(&obj, "basebackup_start_lsn"),
            basebackup_checkpoint_lsn: get_opt_u64(&obj, "basebackup_checkpoint_lsn"),
            basebackup_snapshot_bytes: get_opt_u64(&obj, "basebackup_snapshot_bytes"),
            basebackup_snapshot_checksum: get_opt_u64(&obj, "basebackup_snapshot_checksum"),
            basebackup_manifest: decode_opt_hex(&obj, "basebackup_manifest_hex")?,
            basebackup_chunks,
            basebackup_chunk_ordinal,
            basebackup_chunk: decode_opt_hex(&obj, "basebackup_chunk_hex")?,
        })
    }
}

fn insert_opt_u64(obj: &mut serde_json::Map<String, JsonValue>, field: &str, value: Option<u64>) {
    if let Some(value) = value {
        obj.insert(field.to_string(), JsonValue::Number(value.into()));
    }
}

fn insert_opt_hex(
    obj: &mut serde_json::Map<String, JsonValue>,
    field: &str,
    value: &Option<Vec<u8>>,
) {
    if let Some(bytes) = value {
        obj.insert(field.to_string(), JsonValue::String(hex_encode(bytes)));
    }
}

fn decode_opt_hex(
    obj: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<Option<Vec<u8>>> {
    match obj.get(field).and_then(JsonValue::as_str) {
        Some(value) => Ok(Some(hex_decode(field, value)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basebackup_request_round_trips() {
        let request = BaseBackupRequest {
            replica_id: Some("replica-a".to_string()),
            max_bytes: Some(64),
            snapshot_offset: 128,
            snapshot_token: Some("snapshot:r:1:2".to_string()),
        };
        assert_eq!(
            BaseBackupRequest::decode_json(&request.encode_json()).unwrap(),
            request
        );
    }

    #[test]
    fn basebackup_chunk_round_trips_manifest_and_payload_chunk() {
        let chunk = BaseBackupChunk {
            snapshot_available: true,
            replica_id: "replica-a".to_string(),
            slot_restart_lsn: 7,
            snapshot_lsn: Some(9),
            snapshot_token: Some("snapshot:replica-a:9:100".to_string()),
            snapshot_total_bytes: Some(100),
            snapshot_offset: 0,
            next_snapshot_offset: Some(10),
            snapshot_complete: false,
            snapshot_path: Some("/tmp/replica.rdb".to_string()),
            snapshot_chunk: Some(b"snapshot".to_vec()),
            snapshot_hex: None,
            metadata_binary: Some(b"metadata-binary".to_vec()),
            metadata_json: Some(b"metadata-json".to_vec()),
            header_shadow: Some(b"header-shadow".to_vec()),
            metadata_shadow: Some(b"metadata-shadow".to_vec()),
            basebackup_available: true,
            basebackup_timeline: Some(1),
            basebackup_start_lsn: Some(0),
            basebackup_checkpoint_lsn: Some(9),
            basebackup_snapshot_bytes: Some(100),
            basebackup_snapshot_checksum: Some(123),
            basebackup_manifest: Some(b"manifest".to_vec()),
            basebackup_chunks: vec![BaseBackupManifestChunk {
                ordinal: 0,
                snapshot_offset: 0,
                bytes: 10,
                checksum: 99,
                relative_path: "base/part-000000.redbasepart".to_string(),
            }],
            basebackup_chunk_ordinal: Some(0),
            basebackup_chunk: Some(b"basebackup".to_vec()),
        };
        assert_eq!(
            BaseBackupChunk::decode_json(&chunk.encode_json()).unwrap(),
            chunk
        );
    }
}
