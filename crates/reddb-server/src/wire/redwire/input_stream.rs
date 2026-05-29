//! RedWire input-stream dispatch (issue #764, PRD #759 S5).
//!
//! Brings the S4 HTTP NDJSON input-stream behaviour
//! ([`crate::server::handlers_query::handle_query_ndjson_input_stream`])
//! to the RedWire protocol, reusing the S3 envelope vocabulary:
//!
//!   - `OpenStream` (client→server) — carries `direction: "in"` plus a
//!     `target` table and `columns`. The output-stream variant
//!     (`direction: "out"`, the default) keeps using `sql` and is
//!     handled by [`super::output_stream`]; the two never collide
//!     because the dispatch loop branches on `direction` first.
//!   - `OpenAck`    (server→client) — input stream accepted; carries
//!     the lease handle + snapshot LSN, identical to the output ack.
//!   - `StreamChunk`(client→server) — one chunk of rows. Each chunk
//!     is committed atomically (one multi-row `INSERT`) before the
//!     next frame is read, so rows from chunk K are durable and
//!     visible before chunk K+1 arrives (auto-commit per chunk). A
//!     chunk with `terminal: true` closes the input phase.
//!   - `StreamEnd`  (server→client) — success terminal carrying the
//!     committed RID range (`snapshot_lsn` .. `committed_rid`) and
//!     stats (`row_count`, `chunk_count`).
//!   - `StreamError`(server→client) — a chunk failed to commit. Rows
//!     from earlier chunks remain durable; the error carries
//!     `recoverable_rid` (the CDC LSN at the last good commit) and
//!     the failing `chunk_seq`. No further frames are emitted for the
//!     `stream_id` (AC #3).
//!   - `StreamCancel`(client→server) — discard the in-flight (not yet
//!     committed) chunk; prior committed chunks stay durable (AC #4).
//!
//! Input streams are driven *inline* from the per-connection reader
//! loop (each `StreamChunk` commits synchronously) and tracked in an
//! [`InputStreamRegistry`] keyed by `stream_id`, kept separate from
//! the spawned-worker [`super::output_stream::StreamRegistry`]. Both
//! kinds of stream therefore coexist on one connection, dispatched by
//! `stream_id` (AC #2).

use std::collections::HashMap;

use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};
use reddb_wire::redwire::frame::{Frame, MessageKind};

use super::output_stream::RegisterError;
use super::FrameBuilder;
use crate::server::output_stream::{Clock, OpenStreamError, StreamConfig, StreamLease};

/// `true` when an `OpenStream` payload requests the input direction
/// (`{"direction":"in", ...}`). Any other value — including a missing
/// field or a malformed payload — is treated as the output direction
/// so the existing S3 path keeps owning the default.
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

/// Parsed `OpenStream {direction:"in"}` payload. Shape:
///
/// ```json
/// { "direction": "in", "target": "<table>", "columns": ["c1", "c2"] }
/// ```
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
    use crate::server::handlers_query::is_safe_sql_identifier;
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

/// Parsed `StreamChunk` payload sent by an input-stream client. Shape
/// mirrors the output-stream chunk (`{"seq", "rows", "terminal"}`) but
/// the rows are JSON objects keyed by column rather than already-shaped
/// output rows.
// No `Eq`: `serde_json::Value` rows may carry floats, which are only
// `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct InputChunk {
    pub seq: u64,
    pub rows: Vec<JsonValue>,
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
    // `rows` is optional so a bare terminal frame (`{"terminal":true}`)
    // can close the stream without carrying a final batch.
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

/// Per-stream state for an in-flight input stream. Lives in the
/// session loop's [`InputStreamRegistry`] and is mutated synchronously
/// as each `StreamChunk` is committed.
#[derive(Debug)]
pub struct InputStreamState {
    pub lease: StreamLease,
    pub target: String,
    pub columns: Vec<String>,
    /// CDC LSN at the last successful per-chunk commit; the start of
    /// the committed RID range is the lease's `snapshot_lsn`.
    pub committed_rid: u64,
    pub row_count: u64,
    pub chunk_count: u64,
    pub snapshot_lsn: u64,
}

impl InputStreamState {
    pub fn new(lease: StreamLease, target: String, columns: Vec<String>) -> Self {
        let snapshot_lsn = lease.snapshot_lsn;
        Self {
            lease,
            target,
            columns,
            committed_rid: snapshot_lsn,
            row_count: 0,
            chunk_count: 0,
            snapshot_lsn,
        }
    }

    /// Commit one chunk of rows as a single atomic multi-row `INSERT`.
    /// On success the rows are durable and `committed_rid` advances to
    /// the post-commit CDC LSN. On failure nothing in this chunk
    /// commits — `committed_rid` (and therefore `recoverable_rid`)
    /// stays at the last good commit, so chunks `1..N-1` remain
    /// durable (AC #3).
    pub fn commit_chunk(
        &mut self,
        runtime: &RedDBRuntime,
        rows: &[JsonValue],
    ) -> Result<(), (String, String)> {
        if rows.is_empty() {
            return Ok(());
        }
        // Project each row object onto the declared columns (missing
        // keys become NULL), matching the S4 `parse_row_frame` shape.
        let mut positional: Vec<Vec<JsonValue>> = Vec::with_capacity(rows.len());
        for row in rows {
            let obj = row.as_object().ok_or_else(|| {
                (
                    "invalid_row".to_string(),
                    "row must be a JSON object".to_string(),
                )
            })?;
            let mut values = Vec::with_capacity(self.columns.len());
            for col in &self.columns {
                values.push(obj.get(col).cloned().unwrap_or(JsonValue::Null));
            }
            positional.push(values);
        }
        let sql = crate::server::handlers_query::build_insert_sql(
            &self.target,
            &self.columns,
            &positional,
        )
        .map_err(|message| ("invalid_row".to_string(), message))?;
        match runtime.execute_query(&sql) {
            Ok(_) => {
                self.row_count += rows.len() as u64;
                self.committed_rid = runtime.cdc_current_lsn();
                self.chunk_count += 1;
                Ok(())
            }
            Err(err) => Err(("chunk_commit_failed".to_string(), err.to_string())),
        }
    }
}

/// Per-connection registry of in-flight input streams. Keyed by
/// `stream_id`, separate from the output-stream worker registry so an
/// input and an output stream may share one connection without
/// colliding (AC #2).
#[derive(Default)]
pub struct InputStreamRegistry {
    inner: HashMap<u16, InputStreamState>,
}

impl InputStreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-opened input stream. Mirrors the output
    /// registry's reserved-id / duplicate guards and reuses its
    /// [`RegisterError`] codes so clients see one taxonomy.
    pub fn register(
        &mut self,
        stream_id: u16,
        state: InputStreamState,
    ) -> Result<(), RegisterError> {
        if stream_id == 0 {
            return Err(RegisterError::ReservedStreamId);
        }
        if self.inner.contains_key(&stream_id) {
            return Err(RegisterError::StreamInUse);
        }
        self.inner.insert(stream_id, state);
        Ok(())
    }

    pub fn get_mut(&mut self, stream_id: u16) -> Option<&mut InputStreamState> {
        self.inner.get_mut(&stream_id)
    }

    pub fn contains(&self, stream_id: u16) -> bool {
        self.inner.contains_key(&stream_id)
    }

    /// Drop the stream from the registry, returning its state so the
    /// caller can read final stats for a terminal frame. Idempotent —
    /// a second remove returns `None`.
    pub fn remove(&mut self, stream_id: u16) -> Option<InputStreamState> {
        self.inner.remove(&stream_id)
    }

    pub fn active_count(&self) -> usize {
        self.inner.len()
    }
}

/// Build the success terminal `StreamEnd` payload for an input stream.
/// Carries the committed RID range (`snapshot_lsn` .. `committed_rid`)
/// and ingest stats.
pub fn build_input_stream_end_payload(
    row_count: u64,
    chunk_count: u64,
    committed_rid: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    let mut stats = serde_json::Map::new();
    stats.insert("row_count".to_string(), JsonValue::Number(row_count as f64));
    stats.insert(
        "chunk_count".to_string(),
        JsonValue::Number(chunk_count as f64),
    );
    stats.insert(
        "committed_rid".to_string(),
        JsonValue::Number(committed_rid as f64),
    );
    stats.insert(
        "snapshot_lsn".to_string(),
        JsonValue::Number(snapshot_lsn as f64),
    );
    stats.insert("cancelled".to_string(), JsonValue::Bool(cancelled));
    obj.insert("stats".to_string(), JsonValue::Object(stats));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

/// Build the input-stream `StreamError` payload. Unlike the output
/// variant it carries the `recoverable_rid` prefix (the CDC LSN of the
/// last good commit) and the failing `chunk_seq`.
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
    obj.insert("chunk_seq".to_string(), JsonValue::Number(chunk_seq as f64));
    obj.insert(
        "recoverable_rid".to_string(),
        JsonValue::Number(recoverable_rid as f64),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

/// Build an input-stream `StreamError` frame addressed to `stream_id`,
/// echoing `correlation_id` so the client can pair it to the request.
pub fn build_input_stream_error_frame(
    correlation_id: u64,
    stream_id: u16,
    code: &str,
    message: &str,
    chunk_seq: u64,
    recoverable_rid: u64,
) -> std::io::Result<Frame> {
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
        .map_err(|e| std::io::Error::other(format!("build input StreamError: {e}")))
}

/// Build the input-stream `StreamEnd` frame.
pub fn build_input_stream_end_frame(
    correlation_id: u64,
    stream_id: u16,
    row_count: u64,
    chunk_count: u64,
    committed_rid: u64,
    snapshot_lsn: u64,
    cancelled: bool,
) -> std::io::Result<Frame> {
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
        .map_err(|e| std::io::Error::other(format!("build input StreamEnd: {e}")))
}

/// Open an input-stream lease, reusing the output-stream lease
/// primitive so HTTP, output, and input streams agree on TTL and the
/// in-transaction refusal (AC mirrors S4 #4).
pub fn open_input_lease(
    config: StreamConfig,
    snapshot_lsn: u64,
    in_transaction: bool,
    clock: &dyn Clock,
) -> Result<StreamLease, OpenStreamError> {
    crate::server::output_stream::open_stream(config, snapshot_lsn, in_transaction, clock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_input_direction() {
        assert!(open_stream_is_input(
            br#"{"direction":"in","target":"t","columns":["a"]}"#
        ));
        assert!(open_stream_is_input(br#"{"direction":"IN"}"#));
        // Default / output direction.
        assert!(!open_stream_is_input(br#"{"sql":"SELECT 1"}"#));
        assert!(!open_stream_is_input(br#"{"direction":"out"}"#));
        assert!(!open_stream_is_input(b"not json"));
    }

    #[test]
    fn parse_open_input_accepts_target_and_columns() {
        let req =
            parse_open_input(br#"{"direction":"in","target":"events","columns":["id","name"]}"#)
                .unwrap();
        assert_eq!(req.target, "events");
        assert_eq!(req.columns, vec!["id".to_string(), "name".to_string()]);
    }

    #[test]
    fn parse_open_input_rejects_missing_target() {
        assert!(matches!(
            parse_open_input(br#"{"direction":"in","columns":["a"]}"#),
            Err(OpenInputParseError::MissingTarget)
        ));
    }

    #[test]
    fn parse_open_input_rejects_unsafe_target() {
        assert!(matches!(
            parse_open_input(br#"{"direction":"in","target":"t;DROP","columns":["a"]}"#),
            Err(OpenInputParseError::UnsafeTarget)
        ));
    }

    #[test]
    fn parse_open_input_rejects_empty_or_missing_columns() {
        assert!(matches!(
            parse_open_input(br#"{"direction":"in","target":"t","columns":[]}"#),
            Err(OpenInputParseError::EmptyColumns)
        ));
        assert!(matches!(
            parse_open_input(br#"{"direction":"in","target":"t"}"#),
            Err(OpenInputParseError::MissingColumns)
        ));
    }

    #[test]
    fn parse_open_input_rejects_unsafe_column() {
        assert!(matches!(
            parse_open_input(br#"{"direction":"in","target":"t","columns":["ok","b ad"]}"#),
            Err(OpenInputParseError::UnsafeColumn)
        ));
    }

    #[test]
    fn parse_chunk_extracts_rows_seq_terminal() {
        let chunk =
            parse_input_chunk(br#"{"seq":3,"rows":[{"id":1},{"id":2}],"terminal":true}"#).unwrap();
        assert_eq!(chunk.seq, 3);
        assert_eq!(chunk.rows.len(), 2);
        assert!(chunk.terminal);
    }

    #[test]
    fn parse_chunk_allows_bare_terminal() {
        let chunk = parse_input_chunk(br#"{"terminal":true}"#).unwrap();
        assert!(chunk.rows.is_empty());
        assert!(chunk.terminal);
        assert_eq!(chunk.seq, 0);
    }

    #[test]
    fn parse_chunk_rejects_non_array_rows() {
        assert!(matches!(
            parse_input_chunk(br#"{"rows":5}"#),
            Err(ChunkParseError::RowsNotArray)
        ));
    }

    #[test]
    fn registry_register_rejects_reserved_and_duplicate() {
        let mut reg = InputStreamRegistry::new();
        let lease = StreamLease {
            id: 1,
            lease_handle: "h".to_string(),
            snapshot_lsn: 10,
            opened_at_ms: 0,
            config: StreamConfig::default(),
        };
        assert!(matches!(
            reg.register(
                0,
                InputStreamState::new(
                    StreamLease {
                        id: 2,
                        lease_handle: "h2".to_string(),
                        snapshot_lsn: 10,
                        opened_at_ms: 0,
                        config: StreamConfig::default(),
                    },
                    "t".to_string(),
                    vec!["a".to_string()],
                )
            ),
            Err(RegisterError::ReservedStreamId)
        ));
        reg.register(
            5,
            InputStreamState::new(lease, "t".to_string(), vec!["a".to_string()]),
        )
        .unwrap();
        assert!(reg.contains(5));
        assert!(matches!(
            reg.register(
                5,
                InputStreamState::new(
                    StreamLease {
                        id: 3,
                        lease_handle: "h3".to_string(),
                        snapshot_lsn: 10,
                        opened_at_ms: 0,
                        config: StreamConfig::default(),
                    },
                    "t".to_string(),
                    vec!["a".to_string()],
                )
            ),
            Err(RegisterError::StreamInUse)
        ));
        assert_eq!(reg.active_count(), 1);
        assert!(reg.remove(5).is_some());
        assert!(reg.remove(5).is_none());
    }

    #[test]
    fn end_payload_carries_committed_rid_range_and_stats() {
        let bytes = build_input_stream_end_payload(3, 2, 42, 40, false);
        let v: JsonValue = serde_json::from_slice(&bytes).unwrap();
        let stats = v.as_object().unwrap().get("stats").unwrap();
        assert_eq!(stats.get("row_count").and_then(|x| x.as_u64()), Some(3));
        assert_eq!(stats.get("chunk_count").and_then(|x| x.as_u64()), Some(2));
        assert_eq!(
            stats.get("committed_rid").and_then(|x| x.as_u64()),
            Some(42)
        );
        assert_eq!(stats.get("snapshot_lsn").and_then(|x| x.as_u64()), Some(40));
        assert_eq!(
            stats.get("cancelled").and_then(|x| x.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn error_payload_carries_recoverable_rid_and_chunk_seq() {
        let bytes = build_input_stream_error_payload("invalid_row", "bad", 2, 41);
        let v: JsonValue = serde_json::from_slice(&bytes).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("code").and_then(|x| x.as_str()),
            Some("invalid_row")
        );
        assert_eq!(obj.get("chunk_seq").and_then(|x| x.as_u64()), Some(2));
        assert_eq!(
            obj.get("recoverable_rid").and_then(|x| x.as_u64()),
            Some(41)
        );
    }
}
