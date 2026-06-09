use super::*;

use serde_json::{Map as JsonMap, Value as JsonValue};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SERVERLESS_WRITER_LEASE_DEFAULT_TERM: u64 = 1;
static SERVERLESS_WRITER_LEASE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessWriterLease {
    pub database_key: String,
    pub holder_id: String,
    pub term: u64,
    pub generation: u64,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
}

impl ServerlessWriterLease {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms <= now_ms
    }

    pub fn fenced_by_term(&self, current_term: u64) -> bool {
        self.term < current_term
    }

    pub fn fencing_token(&self) -> (u64, u64) {
        (self.term, self.generation)
    }
}

pub fn serverless_writer_lease_key(prefix: &str, database_key: &str) -> String {
    format!("{prefix}{database_key}.lease.json")
}

pub fn serverless_writer_lease_temp_path(
    kind: &str,
    process_id: u32,
    now_unix_nanos: u128,
    unique: u64,
) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-lease-{kind}-{process_id}-{now_unix_nanos}-{unique}.json"
    ))
}

#[derive(Debug)]
pub struct ServerlessWriterLeaseTempFile {
    path: PathBuf,
}

impl ServerlessWriterLeaseTempFile {
    pub fn new(kind: &str) -> Self {
        let unique = SERVERLESS_WRITER_LEASE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self::with_clock(kind, std::process::id(), now_unix_nanos(), unique)
    }

    pub fn with_clock(kind: &str, process_id: u32, now_unix_nanos: u128, unique: u64) -> Self {
        Self {
            path: serverless_writer_lease_temp_path(kind, process_id, now_unix_nanos, unique),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_bytes(&self, bytes: &[u8]) -> RdbFileResult<()> {
        fs::write(&self.path, bytes)?;
        Ok(())
    }

    pub fn read_bytes(&self) -> RdbFileResult<Vec<u8>> {
        Ok(fs::read(&self.path)?)
    }

    pub fn cleanup(&self) -> RdbFileResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

impl Drop for ServerlessWriterLeaseTempFile {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn now_unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn encode_serverless_writer_lease_json(
    lease: &ServerlessWriterLease,
) -> RdbFileResult<Vec<u8>> {
    let mut object = JsonMap::new();
    object.insert(
        "database_key".to_string(),
        JsonValue::String(lease.database_key.clone()),
    );
    object.insert(
        "holder_id".to_string(),
        JsonValue::String(lease.holder_id.clone()),
    );
    object.insert("term".to_string(), JsonValue::Number(lease.term.into()));
    object.insert(
        "generation".to_string(),
        JsonValue::Number(lease.generation.into()),
    );
    object.insert(
        "acquired_at_ms".to_string(),
        JsonValue::Number(lease.acquired_at_ms.into()),
    );
    object.insert(
        "expires_at_ms".to_string(),
        JsonValue::Number(lease.expires_at_ms.into()),
    );
    serde_json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RdbFileError::InvalidOperation(format!("encode writer lease: {err}")))
}

pub fn decode_serverless_writer_lease_json(bytes: &[u8]) -> RdbFileResult<ServerlessWriterLease> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        RdbFileError::InvalidOperation(format!("decode writer lease json: {err}"))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| RdbFileError::InvalidOperation("lease json is not an object".into()))?;
    Ok(ServerlessWriterLease {
        database_key: required_string(object, "database_key")?,
        holder_id: required_string(object, "holder_id")?,
        term: object
            .get("term")
            .and_then(JsonValue::as_u64)
            .unwrap_or(SERVERLESS_WRITER_LEASE_DEFAULT_TERM),
        generation: required_u64(object, "generation")?,
        acquired_at_ms: required_u64(object, "acquired_at_ms")?,
        expires_at_ms: required_u64(object, "expires_at_ms")?,
    })
}

fn required_string(object: &JsonMap<String, JsonValue>, field: &str) -> RdbFileResult<String> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| RdbFileError::InvalidOperation(format!("missing {field}")))
}

fn required_u64(object: &JsonMap<String, JsonValue>, field: &str) -> RdbFileResult<u64> {
    object
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| RdbFileError::InvalidOperation(format!("missing {field}")))
}
