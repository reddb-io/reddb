//! PostgreSQL wire-protocol listener (Phase 3.1 PG parity).
//!
//! Accepts TCP connections from PG-compatible clients, drives the startup
//! handshake, and routes simple-query frames into the existing
//! `RedDBRuntime::execute_query` path. Results are adapted back into PG
//! `RowDescription` + `DataRow` frames via `types::value_to_pg_wire_bytes`.
//!
//! Phase 3.1 intentionally supports only the simple-query subset; extended
//! query (Parse/Bind/Execute) arrives in 3.1.x once the prepared-statement
//! registry is reusable from this layer.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use super::catalog_views::translate_pg_catalog_query;
use super::protocol::{
    read_frame, read_startup, write_frame, write_raw_byte, BackendMessage, ColumnDescriptor,
    FrontendMessage, PgWireError, TransactionStatus,
};
use super::types::{value_to_pg_wire_bytes, PgOid};
use crate::runtime::ai::ask_response_envelope::{
    AskResult, Citation, Mode, SourceRow, Validation, ValidationError, ValidationWarning,
};
use crate::runtime::RedDBRuntime;
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

/// Startup-tuned configuration for the PG wire listener.
#[derive(Debug, Clone)]
pub struct PgWireConfig {
    /// TCP bind address ("host:port"). The caller is responsible for
    /// keeping this disjoint from the native wire / gRPC / HTTP listeners.
    pub bind_addr: String,
    /// PG version string sent back in `ParameterStatus`. Many drivers
    /// sniff this to enable/disable features. RedDB advertises a
    /// recent-enough version to get the broadest client support.
    pub server_version: String,
}

impl Default for PgWireConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:5432".to_string(),
            server_version: "15.0 (RedDB 3.1)".to_string(),
        }
    }
}

/// Spawn the PG wire listener. Blocks until the listener errors out.
/// Each connection is handled in its own tokio task.
pub async fn start_pg_wire_listener(
    config: PgWireConfig,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(
        transport = "pg-wire",
        bind = %config.bind_addr,
        "listener online"
    );
    let cfg = Arc::new(config);
    loop {
        let (stream, peer) = listener.accept().await?;
        let rt = Arc::clone(&runtime);
        let cfg = Arc::clone(&cfg);
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, rt, cfg).await {
                tracing::warn!(
                    transport = "pg-wire",
                    peer = %peer_str,
                    err = %e,
                    "connection failed"
                );
            }
        });
    }
}

/// Drive one connection's lifetime: startup → authentication → query loop.
pub(crate) async fn handle_connection<S>(
    mut stream: S,
    runtime: Arc<RedDBRuntime>,
    config: Arc<PgWireConfig>,
) -> Result<(), PgWireError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Handshake. The first frame may be SSLRequest / GSSENCRequest
    // (pre-auth negotiation) or a plain Startup. Loop once to cover
    // SSL-not-supported path: reply 'N' and expect the client to send
    // a regular Startup next.
    loop {
        match read_startup(&mut stream).await? {
            FrontendMessage::SslRequest | FrontendMessage::GssEncRequest => {
                // 'N' = not supported — client continues in plaintext and
                // re-sends a normal Startup on the same socket.
                write_raw_byte(&mut stream, b'N').await?;
                continue;
            }
            FrontendMessage::Startup(params) => {
                send_auth_ok(&mut stream, &config, &params).await?;
                break;
            }
            FrontendMessage::Unknown { .. } => {
                // CancelRequest: no response expected; drop the socket.
                return Ok(());
            }
            other => {
                return Err(PgWireError::Protocol(format!(
                    "unexpected startup frame: {other:?}"
                )));
            }
        }
    }

    // Main query loop.
    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(PgWireError::Eof) => return Ok(()),
            Err(e) => return Err(e),
        };

        match frame {
            FrontendMessage::Query(sql) => {
                handle_simple_query(&mut stream, &runtime, &sql).await?;
            }
            FrontendMessage::Terminate => return Ok(()),
            FrontendMessage::Sync | FrontendMessage::Flush => {
                // These are part of the extended protocol. For simple-query
                // sessions we still echo ReadyForQuery so robust clients
                // (that mix S/H frames defensively) keep moving.
                write_frame(
                    &mut stream,
                    &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
                )
                .await?;
            }
            FrontendMessage::PasswordMessage(_) => {
                // Should only arrive during auth. Ignore post-auth.
                continue;
            }
            FrontendMessage::Unknown { tag, .. } => {
                send_error(
                    &mut stream,
                    "0A000",
                    &format!("unsupported frame tag 0x{tag:02x}"),
                )
                .await?;
                write_frame(
                    &mut stream,
                    &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
                )
                .await?;
            }
            other => {
                send_error(
                    &mut stream,
                    "0A000",
                    &format!("unsupported frame {other:?}"),
                )
                .await?;
                write_frame(
                    &mut stream,
                    &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
                )
                .await?;
            }
        }
    }
}

async fn send_auth_ok<S>(
    stream: &mut S,
    config: &PgWireConfig,
    params: &super::protocol::StartupParams,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    // Phase 3.1: trust auth. We always send AuthenticationOk.
    write_frame(stream, &BackendMessage::AuthenticationOk).await?;

    // Standard ParameterStatus frames. Drivers gate capabilities on these.
    for (name, value) in [
        ("server_version", config.server_version.as_str()),
        ("server_encoding", "UTF8"),
        ("client_encoding", "UTF8"),
        ("DateStyle", "ISO, MDY"),
        ("TimeZone", "UTC"),
        ("integer_datetimes", "on"),
        ("standard_conforming_strings", "on"),
        (
            "application_name",
            params.get("application_name").unwrap_or(""),
        ),
    ] {
        write_frame(
            stream,
            &BackendMessage::ParameterStatus {
                name: name.to_string(),
                value: value.to_string(),
            },
        )
        .await?;
    }

    // BackendKeyData: (pid, secret_key). Used by CancelRequest; we don't
    // honour cancels in 3.1 so random-ish values are fine.
    write_frame(
        stream,
        &BackendMessage::BackendKeyData {
            pid: std::process::id(),
            key: 0xDEADBEEF,
        },
    )
    .await?;

    write_frame(
        stream,
        &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
    )
    .await?;
    Ok(())
}

async fn handle_simple_query<S>(
    stream: &mut S,
    runtime: &RedDBRuntime,
    sql: &str,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    // Empty query convention: PG emits EmptyQueryResponse instead of a
    // CommandComplete. Some clients (psql `\;`) rely on this.
    if sql.trim().is_empty() {
        write_frame(stream, &BackendMessage::EmptyQueryResponse).await?;
        write_frame(
            stream,
            &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
        )
        .await?;
        return Ok(());
    }

    let query_result = match translate_pg_catalog_query(runtime, sql) {
        Ok(Some(result)) => Ok(crate::runtime::RuntimeQueryResult {
            query: sql.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "select",
            engine: "pg-catalog",
            result,
            affected_rows: 0,
            statement_type: "select",
        }),
        Ok(None) => runtime.execute_query(sql),
        Err(err) => Err(err),
    };

    match query_result {
        Ok(result) => {
            emit_success_result(stream, &result).await?;
        }
        Err(err) => {
            // PG SQLSTATE class 42 covers syntax / binding errors; we use
            // 42P01 (undefined_table) and 42601 (syntax_error) when we can
            // detect; otherwise fall back to XX000 (internal error).
            let code = classify_sqlstate(&err.to_string());
            send_error(stream, code, &err.to_string()).await?;
        }
    }

    write_frame(
        stream,
        &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
    )
    .await?;
    Ok(())
}

async fn emit_success_result<S>(
    stream: &mut S,
    result: &crate::runtime::RuntimeQueryResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    if result.statement == "ask" {
        emit_ask_result_row(stream, result).await?;
        write_frame(
            stream,
            &BackendMessage::CommandComplete("SELECT 1".to_string()),
        )
        .await?;
    } else if result.statement_type == "select" {
        emit_result_rows(stream, &result.result).await?;
        write_frame(
            stream,
            &BackendMessage::CommandComplete(format!("SELECT {}", result.result.records.len())),
        )
        .await?;
    } else {
        // DDL / DML / config statements: echo the runtime's
        // high-level statement tag back. PG format is
        // "<CMD> [<OID>] <COUNT>"; we keep the count where
        // applicable and fall back to the runtime's message.
        let tag = match result.statement_type {
            "insert" => format!("INSERT 0 {}", result.affected_rows),
            "update" => format!("UPDATE {}", result.affected_rows),
            "delete" => format!("DELETE {}", result.affected_rows),
            other => other.to_uppercase(),
        };
        write_frame(stream, &BackendMessage::CommandComplete(tag)).await?;
    }
    Ok(())
}

async fn emit_result_rows<S>(
    stream: &mut S,
    result: &crate::storage::query::unified::UnifiedResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    // RowDescription: derived from the first record's column ordering.
    // When `result.columns` is non-empty we honour that order; otherwise
    // we synthesise one from the record's field order.
    let columns: Vec<String> = if !result.columns.is_empty() {
        result.columns.clone()
    } else if let Some(first) = result.records.first() {
        record_field_names(first)
    } else {
        Vec::new()
    };

    // Peek at the first record for per-column type OIDs. When there's no
    // data row we fall back to TEXT for every column — clients render
    // empty result sets happily.
    let type_oids: Vec<PgOid> = columns
        .iter()
        .map(|col| {
            result
                .records
                .first()
                .and_then(|r| record_get(r, col))
                .map(PgOid::from_value)
                .unwrap_or(PgOid::Text)
        })
        .collect();

    let descriptors: Vec<ColumnDescriptor> = columns
        .iter()
        .zip(type_oids.iter())
        .map(|(name, oid)| ColumnDescriptor {
            name: name.clone(),
            table_oid: 0,
            column_attr: 0,
            type_oid: oid.as_u32(),
            type_size: -1,
            type_mod: -1,
            format: 0,
        })
        .collect();

    write_frame(stream, &BackendMessage::RowDescription(descriptors)).await?;

    for record in &result.records {
        let fields: Vec<Option<Vec<u8>>> = columns
            .iter()
            .map(|col| record_get(record, col).and_then(value_to_pg_wire_bytes))
            .collect();
        write_frame(stream, &BackendMessage::DataRow(fields)).await?;
    }

    Ok(())
}

async fn emit_ask_result_row<S>(
    stream: &mut S,
    result: &crate::runtime::RuntimeQueryResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    let row = ask_query_result_to_pg_wire_row(result)
        .ok_or_else(|| PgWireError::Protocol("ASK result missing row body".to_string()))?;
    let descriptors: Vec<ColumnDescriptor> = row
        .columns
        .iter()
        .map(|col| ColumnDescriptor {
            name: col.name.to_string(),
            table_oid: 0,
            column_attr: 0,
            type_oid: col.oid.as_u32(),
            type_size: -1,
            type_mod: -1,
            format: 0,
        })
        .collect();

    write_frame(stream, &BackendMessage::RowDescription(descriptors)).await?;
    write_frame(stream, &BackendMessage::DataRow(row.cells)).await?;
    Ok(())
}

fn ask_query_result_to_pg_wire_row(
    result: &crate::runtime::RuntimeQueryResult,
) -> Option<crate::runtime::ai::pg_wire_ask_row_encoder::AskRow> {
    if result.statement != "ask" {
        return None;
    }
    let record = result.result.records.first()?;
    let sources_flat_json =
        json_field(record, "sources_flat").unwrap_or(crate::json::Value::Array(Vec::new()));
    let citations_json =
        json_field(record, "citations").unwrap_or(crate::json::Value::Array(Vec::new()));
    let validation_json = json_field(record, "validation")
        .unwrap_or_else(|| crate::json::Value::Object(Default::default()));

    let effective_mode = match text_field(record, "mode").as_deref() {
        Some("lenient") => Mode::Lenient,
        _ => Mode::Strict,
    };

    let ask = AskResult {
        answer: text_field(record, "answer")?,
        sources_flat: ask_sources_flat(&sources_flat_json),
        citations: ask_citations(&citations_json),
        validation: ask_validation(&validation_json),
        cache_hit: bool_field(record, "cache_hit").unwrap_or(false),
        provider: text_field(record, "provider").unwrap_or_default(),
        model: text_field(record, "model").unwrap_or_default(),
        prompt_tokens: u32_field(record, "prompt_tokens").unwrap_or(0),
        completion_tokens: u32_field(record, "completion_tokens").unwrap_or(0),
        cost_usd: f64_field(record, "cost_usd").unwrap_or(0.0),
        effective_mode,
        retry_count: u32_field(record, "retry_count").unwrap_or(0),
    };

    Some(crate::runtime::ai::pg_wire_ask_row_encoder::encode(&ask))
}

fn record_field<'a>(record: &'a UnifiedRecord, key: &str) -> Option<&'a Value> {
    record.iter_fields().find_map(|(name, value)| {
        let name: &str = name;
        (name == key).then_some(value)
    })
}

fn text_field(record: &UnifiedRecord, key: &str) -> Option<String> {
    match record_field(record, key)? {
        Value::Text(s) => Some(s.to_string()),
        Value::Email(s) | Value::Url(s) | Value::NodeRef(s) | Value::EdgeRef(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn bool_field(record: &UnifiedRecord, key: &str) -> Option<bool> {
    match record_field(record, key)? {
        Value::Boolean(value) => Some(*value),
        _ => None,
    }
}

fn u32_field(record: &UnifiedRecord, key: &str) -> Option<u32> {
    match record_field(record, key)? {
        Value::Integer(n) => (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32),
        Value::UnsignedInteger(n) => Some((*n).min(u32::MAX as u64) as u32),
        Value::BigInt(n)
        | Value::TimestampMs(n)
        | Value::Timestamp(n)
        | Value::Duration(n)
        | Value::Decimal(n) => (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32),
        Value::Float(n) => (*n >= 0.0).then_some((*n).min(u32::MAX as f64) as u32),
        _ => None,
    }
}

fn f64_field(record: &UnifiedRecord, key: &str) -> Option<f64> {
    match record_field(record, key)? {
        Value::Integer(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        Value::BigInt(n)
        | Value::TimestampMs(n)
        | Value::Timestamp(n)
        | Value::Duration(n)
        | Value::Decimal(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    }
}

fn json_field(record: &UnifiedRecord, key: &str) -> Option<crate::json::Value> {
    match record_field(record, key)? {
        Value::Json(bytes) => crate::json::from_slice(bytes).ok(),
        Value::Text(text) => crate::json::from_str(text).ok(),
        _ => None,
    }
}

fn ask_sources_flat(value: &crate::json::Value) -> Vec<SourceRow> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|source| {
            let urn = source
                .get("urn")
                .and_then(crate::json::Value::as_str)?
                .to_string();
            let payload = source
                .get("payload")
                .and_then(crate::json::Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| source.to_string_compact());
            Some(SourceRow { urn, payload })
        })
        .collect()
}

fn ask_citations(value: &crate::json::Value) -> Vec<Citation> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|citation| {
            let marker = citation
                .get("marker")
                .and_then(crate::json::Value::as_u64)?;
            let urn = citation
                .get("urn")
                .and_then(crate::json::Value::as_str)?
                .to_string();
            Some(Citation {
                marker: marker.min(u32::MAX as u64) as u32,
                urn,
            })
        })
        .collect()
}

fn ask_validation(value: &crate::json::Value) -> Validation {
    Validation {
        ok: value
            .get("ok")
            .and_then(crate::json::Value::as_bool)
            .unwrap_or(true),
        warnings: validation_items(value, "warnings")
            .into_iter()
            .map(|(kind, detail)| ValidationWarning { kind, detail })
            .collect(),
        errors: validation_items(value, "errors")
            .into_iter()
            .map(|(kind, detail)| ValidationError { kind, detail })
            .collect(),
    }
}

fn validation_items(value: &crate::json::Value, key: &str) -> Vec<(String, String)> {
    value
        .get(key)
        .and_then(crate::json::Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|item| {
            Some((
                item.get("kind")
                    .and_then(crate::json::Value::as_str)?
                    .to_string(),
                item.get("detail")
                    .and_then(crate::json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect()
}

/// Best-effort field lookup on a `UnifiedRecord`. The record API lives in
/// `storage::query::unified` and today uses `HashMap<String, Value>` under
/// the hood — we use `get` if it exists, else fall back to serialised map.
fn record_get<'a>(record: &'a UnifiedRecord, key: &str) -> Option<&'a Value> {
    record.get(key)
}

/// Extract column names in iteration order from a single record. When
/// the caller didn't supply an explicit `columns` projection we use the
/// first record's field ordering as the canonical tuple shape.
///
/// HashMap iteration order is non-deterministic — for Phase 3.1 we
/// accept the shuffle since PG clients receive the ordered header via
/// RowDescription and match cells positionally. A stable ordering
/// would require keeping an insertion-order index alongside `values`.
fn record_field_names(record: &UnifiedRecord) -> Vec<String> {
    // `column_names()` merges the columnar scan side-channel with
    // the HashMap so scan rows (which populate only columnar) still
    // surface their field names in PG wire output.
    record
        .column_names()
        .into_iter()
        .map(|k| k.to_string())
        .collect()
}

async fn send_error<S>(stream: &mut S, code: &str, message: &str) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    write_frame(
        stream,
        &BackendMessage::ErrorResponse {
            severity: "ERROR".to_string(),
            code: code.to_string(),
            message: message.to_string(),
        },
    )
    .await
}

/// Heuristically map a runtime error message onto a PG SQLSTATE. Full
/// coverage would map every `RedDBError` variant; this is enough for the
/// common psql / JDBC paths.
fn classify_sqlstate(msg: &str) -> &'static str {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("does not exist") {
        // 42P01 undefined_table; close enough for collection-not-found.
        "42P01"
    } else if lower.contains("parse") || lower.contains("expected") || lower.contains("syntax") {
        "42601"
    } else if lower.contains("already exists") {
        "42P07"
    } else if lower.contains("permission") || lower.contains("auth") {
        "28000"
    } else {
        "XX000"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeQueryResult;
    use crate::storage::query::modes::QueryMode;
    use crate::storage::query::unified::UnifiedResult;

    #[tokio::test]
    async fn ask_success_result_uses_canonical_pg_wire_row_shape() {
        let mut result = UnifiedResult::with_columns(vec![
            "answer".into(),
            "provider".into(),
            "model".into(),
            "prompt_tokens".into(),
            "completion_tokens".into(),
            "sources_count".into(),
            "sources_flat".into(),
            "citations".into(),
            "validation".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("answer", Value::text("Deploy failed [^1]."));
        record.set("provider", Value::text("openai"));
        record.set("model", Value::text("gpt-4o-mini"));
        record.set("prompt_tokens", Value::Integer(11));
        record.set("completion_tokens", Value::Integer(7));
        record.set(
            "sources_flat",
            Value::Json(
                br#"[{"urn":"urn:reddb:row:deployments:1","kind":"row","collection":"deployments","id":"1"}]"#
                    .to_vec(),
            ),
        );
        record.set(
            "citations",
            Value::Json(br#"[{"marker":1,"urn":"urn:reddb:row:deployments:1"}]"#.to_vec()),
        );
        record.set(
            "validation",
            Value::Json(br#"{"ok":true,"warnings":[],"errors":[]}"#.to_vec()),
        );
        result.push(record);

        let qr = RuntimeQueryResult {
            query: "ASK 'why did deploy fail?'".to_string(),
            mode: QueryMode::Sql,
            statement: "ask",
            engine: "runtime-ai",
            result,
            affected_rows: 0,
            statement_type: "select",
        };

        let mut out = Vec::new();
        emit_success_result(&mut out, &qr).await.unwrap();
        let frames = decode_frames(&out);

        assert_eq!(
            frames.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            b"TDC"
        );

        let columns = decode_row_description(frames[0].1);
        assert_eq!(
            columns,
            vec![
                ("answer".to_string(), PgOid::Text.as_u32()),
                ("cache_hit".to_string(), PgOid::Bool.as_u32()),
                ("citations".to_string(), PgOid::Jsonb.as_u32()),
                ("completion_tokens".to_string(), PgOid::Int8.as_u32()),
                ("cost_usd".to_string(), PgOid::Numeric.as_u32()),
                ("mode".to_string(), PgOid::Text.as_u32()),
                ("model".to_string(), PgOid::Text.as_u32()),
                ("prompt_tokens".to_string(), PgOid::Int8.as_u32()),
                ("provider".to_string(), PgOid::Text.as_u32()),
                ("retry_count".to_string(), PgOid::Int8.as_u32()),
                ("sources_flat".to_string(), PgOid::Jsonb.as_u32()),
                ("validation".to_string(), PgOid::Jsonb.as_u32()),
            ]
        );

        let cells = decode_data_row(frames[1].1);
        assert_eq!(cells.len(), 12);
        assert_eq!(cells[0].as_deref(), Some(b"Deploy failed [^1].".as_slice()));
        assert_eq!(cells[1].as_deref(), Some(b"f".as_slice()));
        assert_eq!(cells[4].as_deref(), Some(b"0".as_slice()));
        assert_eq!(cells[5].as_deref(), Some(b"strict".as_slice()));
        assert_eq!(cells[9].as_deref(), Some(b"0".as_slice()));
        assert!(std::str::from_utf8(cells[10].as_deref().unwrap())
            .unwrap()
            .contains(r#""payload""#));
        assert_eq!(decode_command_complete(frames[2].1), "SELECT 1");
    }

    fn decode_frames(bytes: &[u8]) -> Vec<(u8, &[u8])> {
        let mut pos = 0;
        let mut frames = Vec::new();
        while pos < bytes.len() {
            let tag = bytes[pos];
            let len = u32::from_be_bytes([
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
                bytes[pos + 4],
            ]) as usize;
            let body_start = pos + 5;
            let body_end = pos + 1 + len;
            frames.push((tag, &bytes[body_start..body_end]));
            pos = body_end;
        }
        frames
    }

    fn decode_row_description(body: &[u8]) -> Vec<(String, u32)> {
        let count = i16::from_be_bytes([body[0], body[1]]) as usize;
        let mut pos = 2;
        let mut columns = Vec::with_capacity(count);
        for _ in 0..count {
            let end = body[pos..].iter().position(|&b| b == 0).unwrap() + pos;
            let name = std::str::from_utf8(&body[pos..end]).unwrap().to_string();
            pos = end + 1;
            pos += 4; // table oid
            pos += 2; // column attr
            let oid = u32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
            pos += 4;
            pos += 2; // type size
            pos += 4; // type mod
            pos += 2; // format
            columns.push((name, oid));
        }
        columns
    }

    fn decode_data_row(body: &[u8]) -> Vec<Option<Vec<u8>>> {
        let count = i16::from_be_bytes([body[0], body[1]]) as usize;
        let mut pos = 2;
        let mut cells = Vec::with_capacity(count);
        for _ in 0..count {
            let len = i32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
            pos += 4;
            if len < 0 {
                cells.push(None);
            } else {
                let len = len as usize;
                cells.push(Some(body[pos..pos + len].to_vec()));
                pos += len;
            }
        }
        cells
    }

    fn decode_command_complete(body: &[u8]) -> &str {
        let nul = body.iter().position(|&b| b == 0).unwrap_or(body.len());
        std::str::from_utf8(&body[..nul]).unwrap()
    }
}
