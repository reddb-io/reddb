//! RedWire stream payload contracts.

use serde_json::Value as JsonValue;

use super::{BuildError, Frame, FrameBuilder, MessageKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStreamRequest {
    pub sql: String,
    pub opts_raw: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenStreamParseError {
    NotJson,
    NotObject,
    MissingSql,
    EmptySql,
}

impl OpenStreamParseError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotJson | Self::NotObject => "open_stream_invalid_payload",
            Self::MissingSql | Self::EmptySql => "open_stream_missing_sql",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::NotJson => "OpenStream payload must be JSON",
            Self::NotObject => "OpenStream payload must be a JSON object",
            Self::MissingSql => "OpenStream payload missing 'sql' string field",
            Self::EmptySql => "OpenStream payload 'sql' must be non-empty",
        }
    }
}

pub fn parse_open_stream(payload: &[u8]) -> Result<OpenStreamRequest, OpenStreamParseError> {
    let v: JsonValue =
        serde_json::from_slice(payload).map_err(|_| OpenStreamParseError::NotJson)?;
    let obj = v.as_object().ok_or(OpenStreamParseError::NotObject)?;
    let sql = obj
        .get("sql")
        .and_then(|x| x.as_str())
        .ok_or(OpenStreamParseError::MissingSql)?;
    if sql.is_empty() {
        return Err(OpenStreamParseError::EmptySql);
    }
    let opts_raw = obj
        .get("opts")
        .map(|v| serde_json::to_vec(v).unwrap_or_default())
        .unwrap_or_default();
    Ok(OpenStreamRequest {
        sql: sql.to_string(),
        opts_raw,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamCancelRequest {
    pub reason: Option<String>,
}

pub fn parse_stream_cancel(payload: &[u8]) -> StreamCancelRequest {
    if payload.is_empty() {
        return StreamCancelRequest::default();
    }
    let v: JsonValue = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(_) => return StreamCancelRequest::default(),
    };
    let reason = v
        .as_object()
        .and_then(|o| o.get("reason"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    StreamCancelRequest { reason }
}

pub fn build_open_ack_payload(lease_id: u64, snapshot_lsn: u64, resumable: bool) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "lease_handle".to_string(),
        JsonValue::String(lease_id.to_string()),
    );
    obj.insert("resumable".to_string(), JsonValue::Bool(resumable));
    obj.insert(
        "snapshot_lsn".to_string(),
        JsonValue::Number(snapshot_lsn.into()),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_open_ack_frame(
    correlation_id: u64,
    stream_id: u16,
    lease_id: u64,
    snapshot_lsn: u64,
    resumable: bool,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::OpenAck)
        .stream_id(stream_id)
        .payload(build_open_ack_payload(lease_id, snapshot_lsn, resumable))
        .build()
}

pub fn build_stream_chunk_payload(seq: u64, rows: Vec<JsonValue>, terminal: bool) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("seq".to_string(), JsonValue::Number(seq.into()));
    obj.insert("rows".to_string(), JsonValue::Array(rows));
    obj.insert("terminal".to_string(), JsonValue::Bool(terminal));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_stream_chunk_payload_from_json_bytes(
    seq: u64,
    rows: Vec<Vec<u8>>,
    terminal: bool,
) -> Vec<u8> {
    let rows = rows
        .into_iter()
        .map(|row| serde_json::from_slice(&row).unwrap_or(JsonValue::Null))
        .collect();
    build_stream_chunk_payload(seq, rows, terminal)
}

pub fn build_stream_chunk_frame_from_json_bytes(
    correlation_id: u64,
    stream_id: u16,
    seq: u64,
    rows: Vec<Vec<u8>>,
    terminal: bool,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamChunk)
        .stream_id(stream_id)
        .payload(build_stream_chunk_payload_from_json_bytes(
            seq, rows, terminal,
        ))
        .build()
}

pub fn build_stream_error_payload(seq: Option<u64>, code: &str, message: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    if let Some(s) = seq {
        obj.insert("seq".to_string(), JsonValue::Number(s.into()));
    }
    obj.insert("code".to_string(), JsonValue::String(code.to_string()));
    obj.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_stream_error_frame(
    correlation_id: u64,
    stream_id: u16,
    seq: Option<u64>,
    code: &str,
    message: &str,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamError)
        .stream_id(stream_id)
        .payload(build_stream_error_payload(seq, code, message))
        .build()
}

pub fn build_stream_end_payload(
    row_count: u64,
    lease_id: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    let mut stats = serde_json::Map::new();
    stats.insert("row_count".to_string(), JsonValue::Number(row_count.into()));
    stats.insert("lease_id".to_string(), JsonValue::Number(lease_id.into()));
    stats.insert(
        "snapshot_lsn".to_string(),
        JsonValue::Number(snapshot_lsn.into()),
    );
    stats.insert("cancelled".to_string(), JsonValue::Bool(cancelled));
    obj.insert("stats".to_string(), JsonValue::Object(stats));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_stream_end_frame(
    correlation_id: u64,
    stream_id: u16,
    row_count: u64,
    lease_id: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamEnd)
        .stream_id(stream_id)
        .payload(build_stream_end_payload(
            row_count,
            lease_id,
            snapshot_lsn,
            cancelled,
        ))
        .build()
}

pub fn open_stream_is_input(payload: &[u8]) -> bool {
    serde_json::from_slice::<JsonValue>(payload)
        .ok()
        .and_then(|v| {
            v.as_object()
                .and_then(|o| o.get("direction"))
                .and_then(|d| d.as_str())
                .map(|s| s.eq_ignore_ascii_case("in"))
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenInputRequest {
    pub target: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenInputParseError {
    NotJson,
    NotObject,
    MissingTarget,
    UnsafeTarget,
    MissingColumns,
    EmptyColumns,
    UnsafeColumn,
}

impl OpenInputParseError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotJson | Self::NotObject => "open_stream_invalid_payload",
            Self::MissingTarget | Self::UnsafeTarget => "open_stream_invalid_target",
            Self::MissingColumns | Self::EmptyColumns | Self::UnsafeColumn => {
                "open_stream_invalid_columns"
            }
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::NotJson => "OpenStream payload must be JSON",
            Self::NotObject => "OpenStream payload must be a JSON object",
            Self::MissingTarget => "input OpenStream payload missing 'target' string field",
            Self::UnsafeTarget => "input OpenStream 'target' is not a safe SQL identifier",
            Self::MissingColumns => "input OpenStream payload missing 'columns' array field",
            Self::EmptyColumns => "input OpenStream 'columns' must be a non-empty array",
            Self::UnsafeColumn => "input OpenStream 'columns' entry is not a safe SQL identifier",
        }
    }
}

pub fn parse_open_input(payload: &[u8]) -> Result<OpenInputRequest, OpenInputParseError> {
    let v: JsonValue = serde_json::from_slice(payload).map_err(|_| OpenInputParseError::NotJson)?;
    let obj = v.as_object().ok_or(OpenInputParseError::NotObject)?;
    let target = obj
        .get("target")
        .and_then(|x| x.as_str())
        .ok_or(OpenInputParseError::MissingTarget)?;
    if !is_safe_sql_identifier(target) {
        return Err(OpenInputParseError::UnsafeTarget);
    }
    let columns_v = obj
        .get("columns")
        .and_then(|x| x.as_array())
        .ok_or(OpenInputParseError::MissingColumns)?;
    if columns_v.is_empty() {
        return Err(OpenInputParseError::EmptyColumns);
    }
    let mut columns = Vec::with_capacity(columns_v.len());
    for c in columns_v {
        let name = c.as_str().ok_or(OpenInputParseError::UnsafeColumn)?;
        if !is_safe_sql_identifier(name) {
            return Err(OpenInputParseError::UnsafeColumn);
        }
        columns.push(name.to_string());
    }
    Ok(OpenInputRequest {
        target: target.to_string(),
        columns,
    })
}

fn is_safe_sql_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputChunk {
    pub seq: u64,
    pub rows: Vec<JsonValue>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChunkJson {
    pub seq: u64,
    pub rows_json: Vec<Vec<u8>>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkParseError {
    NotJson,
    NotObject,
    RowsNotArray,
}

impl ChunkParseError {
    pub fn code(&self) -> &'static str {
        "invalid_chunk"
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::NotJson => "StreamChunk payload must be JSON",
            Self::NotObject => "StreamChunk payload must be a JSON object",
            Self::RowsNotArray => "StreamChunk 'rows' must be an array",
        }
    }
}

pub fn parse_input_chunk(payload: &[u8]) -> Result<InputChunk, ChunkParseError> {
    let v: JsonValue = serde_json::from_slice(payload).map_err(|_| ChunkParseError::NotJson)?;
    let obj = v.as_object().ok_or(ChunkParseError::NotObject)?;
    let seq = obj.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
    let terminal = obj
        .get("terminal")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let rows = match obj.get("rows") {
        None | Some(JsonValue::Null) => Vec::new(),
        Some(JsonValue::Array(arr)) => arr.clone(),
        Some(_) => return Err(ChunkParseError::RowsNotArray),
    };
    Ok(InputChunk {
        seq,
        rows,
        terminal,
    })
}

pub fn parse_input_chunk_json(payload: &[u8]) -> Result<InputChunkJson, ChunkParseError> {
    let chunk = parse_input_chunk(payload)?;
    let rows_json = chunk
        .rows
        .iter()
        .map(|row| serde_json::to_vec(row).unwrap_or_default())
        .collect();
    Ok(InputChunkJson {
        seq: chunk.seq,
        rows_json,
        terminal: chunk.terminal,
    })
}

pub fn build_input_stream_end_payload(
    row_count: u64,
    chunk_count: u64,
    committed_rid: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    let mut stats = serde_json::Map::new();
    stats.insert("row_count".to_string(), JsonValue::Number(row_count.into()));
    stats.insert(
        "chunk_count".to_string(),
        JsonValue::Number(chunk_count.into()),
    );
    stats.insert(
        "committed_rid".to_string(),
        JsonValue::Number(committed_rid.into()),
    );
    stats.insert(
        "snapshot_lsn".to_string(),
        JsonValue::Number(snapshot_lsn.into()),
    );
    stats.insert("cancelled".to_string(), JsonValue::Bool(cancelled));
    obj.insert("stats".to_string(), JsonValue::Object(stats));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_input_stream_end_frame(
    correlation_id: u64,
    stream_id: u16,
    row_count: u64,
    chunk_count: u64,
    committed_rid: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamEnd)
        .stream_id(stream_id)
        .payload(build_input_stream_end_payload(
            row_count,
            chunk_count,
            committed_rid,
            snapshot_lsn,
            cancelled,
        ))
        .build()
}

pub fn build_input_stream_error_payload(
    code: &str,
    message: &str,
    chunk_seq: u64,
    recoverable_rid: u64,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("code".to_string(), JsonValue::String(code.to_string()));
    obj.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );
    obj.insert("chunk_seq".to_string(), JsonValue::Number(chunk_seq.into()));
    obj.insert(
        "recoverable_rid".to_string(),
        JsonValue::Number(recoverable_rid.into()),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_input_stream_error_frame(
    correlation_id: u64,
    stream_id: u16,
    code: &str,
    message: &str,
    chunk_seq: u64,
    recoverable_rid: u64,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamError)
        .stream_id(stream_id)
        .payload(build_input_stream_error_payload(
            code,
            message,
            chunk_seq,
            recoverable_rid,
        ))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_open_stream_contract_parses_opts() {
        let req = parse_open_stream(br#"{"sql":"SELECT 1","opts":{"resume_after_rid":42}}"#)
            .expect("parse open stream");
        assert_eq!(req.sql, "SELECT 1");
        assert!(!req.opts_raw.is_empty());
    }

    #[test]
    fn input_open_contract_rejects_unsafe_identifiers() {
        assert_eq!(
            parse_open_input(br#"{"direction":"in","target":"t;drop","columns":["id"]}"#),
            Err(OpenInputParseError::UnsafeTarget)
        );
        assert_eq!(
            parse_open_input(br#"{"direction":"in","target":"t","columns":["bad name"]}"#),
            Err(OpenInputParseError::UnsafeColumn)
        );
    }

    #[test]
    fn input_chunk_json_preserves_rows_as_json_bytes() {
        let chunk =
            parse_input_chunk_json(br#"{"seq":3,"rows":[{"id":1}],"terminal":true}"#).unwrap();
        assert_eq!(chunk.seq, 3);
        assert_eq!(chunk.rows_json.len(), 1);
        assert!(std::str::from_utf8(&chunk.rows_json[0])
            .unwrap()
            .contains("\"id\""));
        assert!(chunk.terminal);
    }

    #[test]
    fn stream_payload_builders_emit_json_objects() {
        let ack = build_open_ack_payload(42, 7, false);
        let value: JsonValue = serde_json::from_slice(&ack).unwrap();
        assert_eq!(value["lease_handle"], "42");

        let end = build_stream_end_payload(5, 42, 7, true);
        let value: JsonValue = serde_json::from_slice(&end).unwrap();
        assert_eq!(value["stats"]["cancelled"], true);
    }

    #[test]
    fn stream_frame_builders_echo_stream_and_correlation() {
        let ack = build_open_ack_frame(99, 7, 42, 100, false).unwrap();
        assert_eq!(ack.kind, MessageKind::OpenAck);
        assert_eq!(ack.correlation_id, 99);
        assert_eq!(ack.stream_id, 7);

        let chunk = build_stream_chunk_frame_from_json_bytes(
            99,
            7,
            1,
            vec![br#"{"id":1}"#.to_vec()],
            false,
        )
        .unwrap();
        assert_eq!(chunk.kind, MessageKind::StreamChunk);
        assert_eq!(chunk.stream_id, 7);

        let error = build_stream_error_frame(99, 7, Some(1), "bad", "failed").unwrap();
        assert_eq!(error.kind, MessageKind::StreamError);
        assert_eq!(error.correlation_id, 99);

        let end = build_stream_end_frame(99, 7, 5, 42, 100, true).unwrap();
        assert_eq!(end.kind, MessageKind::StreamEnd);
        assert_eq!(end.stream_id, 7);

        let input_error =
            build_input_stream_error_frame(99, 8, "invalid_row", "bad", 2, 41).unwrap();
        assert_eq!(input_error.kind, MessageKind::StreamError);
        assert_eq!(input_error.stream_id, 8);

        let input_end = build_input_stream_end_frame(99, 8, 3, 2, 42, 40, false).unwrap();
        assert_eq!(input_end.kind, MessageKind::StreamEnd);
        assert_eq!(input_end.correlation_id, 99);
    }
}
