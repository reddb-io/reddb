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

use super::protocol::{
    read_frame, read_startup, write_frame, write_raw_byte, BackendMessage, ColumnDescriptor,
    FrontendMessage, PgWireError, TransactionStatus,
};
use super::types::{value_to_pg_wire_bytes, PgOid};
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

    match runtime.execute_query(sql) {
        Ok(result) => {
            if result.statement_type == "select" {
                emit_result_rows(stream, &result.result).await?;
                write_frame(
                    stream,
                    &BackendMessage::CommandComplete(format!(
                        "SELECT {}",
                        result.result.records.len()
                    )),
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
