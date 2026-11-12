use super::*;

use serde_json::{Map as JsonMap, Value as JsonValue};

pub const SERVERLESS_SNAPSHOT_HEAD_FORMAT_VERSION: u64 = 1;

/// Canonical pointer for lease-fenced serverless snapshot publication.
///
/// Snapshot payloads are immutable. Writers compete only on this small head
/// object with backend CAS, so a writer from an older lease generation may
/// finish its payload upload but cannot make that payload current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessSnapshotHead {
    pub database_key: String,
    pub holder_id: String,
    pub term: u64,
    pub lease_generation: u64,
    pub publication_sequence: u64,
    pub snapshot_key: String,
}

impl ServerlessSnapshotHead {
    pub fn fencing_token(&self) -> (u64, u64) {
        (self.term, self.lease_generation)
    }
}

pub fn serverless_snapshot_head_key(database_key: &str) -> String {
    format!("{database_key}.serverless/HEAD.json")
}

pub fn serverless_snapshot_key(
    database_key: &str,
    lease_generation: u64,
    publication_sequence: u64,
) -> String {
    format!("{database_key}.serverless/snapshots/{lease_generation}/{publication_sequence}.rdb")
}

pub fn encode_serverless_snapshot_head_json(
    head: &ServerlessSnapshotHead,
) -> RdbFileResult<Vec<u8>> {
    let mut object = JsonMap::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(SERVERLESS_SNAPSHOT_HEAD_FORMAT_VERSION.into()),
    );
    object.insert(
        "database_key".to_string(),
        JsonValue::String(head.database_key.clone()),
    );
    object.insert(
        "holder_id".to_string(),
        JsonValue::String(head.holder_id.clone()),
    );
    object.insert("term".to_string(), JsonValue::Number(head.term.into()));
    object.insert(
        "lease_generation".to_string(),
        JsonValue::Number(head.lease_generation.into()),
    );
    object.insert(
        "publication_sequence".to_string(),
        JsonValue::Number(head.publication_sequence.into()),
    );
    object.insert(
        "snapshot_key".to_string(),
        JsonValue::String(head.snapshot_key.clone()),
    );
    serde_json::to_vec(&JsonValue::Object(object)).map_err(|err| {
        RdbFileError::InvalidOperation(format!("encode serverless snapshot head: {err}"))
    })
}

pub fn decode_serverless_snapshot_head_json(bytes: &[u8]) -> RdbFileResult<ServerlessSnapshotHead> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        RdbFileError::InvalidOperation(format!("decode serverless snapshot head: {err}"))
    })?;
    let object = value.as_object().ok_or_else(|| {
        RdbFileError::InvalidOperation("serverless snapshot head is not an object".into())
    })?;
    let format_version = required_u64(object, "format_version")?;
    if format_version != SERVERLESS_SNAPSHOT_HEAD_FORMAT_VERSION {
        return Err(RdbFileError::InvalidOperation(format!(
            "unsupported serverless snapshot head version {format_version}"
        )));
    }
    Ok(ServerlessSnapshotHead {
        database_key: required_string(object, "database_key")?,
        holder_id: required_string(object, "holder_id")?,
        term: required_u64(object, "term")?,
        lease_generation: required_u64(object, "lease_generation")?,
        publication_sequence: required_u64(object, "publication_sequence")?,
        snapshot_key: required_string(object, "snapshot_key")?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_head_round_trips_with_fencing_token() {
        let head = ServerlessSnapshotHead {
            database_key: "databases/main.rdb".to_string(),
            holder_id: "writer-a".to_string(),
            term: 3,
            lease_generation: 7,
            publication_sequence: 11,
            snapshot_key: "databases/main.rdb.serverless/snapshots/7/11.rdb".to_string(),
        };

        let decoded = decode_serverless_snapshot_head_json(
            &encode_serverless_snapshot_head_json(&head).expect("encode head"),
        )
        .expect("decode head");

        assert_eq!(decoded, head);
        assert_eq!(decoded.fencing_token(), (3, 7));
    }
}
