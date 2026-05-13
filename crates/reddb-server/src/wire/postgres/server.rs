//! PostgreSQL wire-protocol listener (Phase 3.1 PG parity).
//!
//! Accepts TCP connections from PG-compatible clients, drives the startup
//! handshake, and routes simple/extended-query frames into the existing
//! `RedDBRuntime::execute_query` path. Results are adapted back into PG
//! `RowDescription` + `DataRow` frames via `types::value_to_pg_wire_bytes`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use super::catalog_views::translate_pg_catalog_query;
use super::protocol::{
    read_frame, read_startup, write_frame, write_raw_byte, BackendMessage, ColumnDescriptor,
    DescribeTarget, FrontendMessage, PgWireError, TransactionStatus,
};
use super::types::{pg_param_to_value, value_to_pg_wire_bytes, PgOid};
use crate::runtime::ai::ask_response_envelope::{
    AskResult, Citation, Mode, SourceRow, Validation, ValidationError, ValidationWarning,
};
use crate::runtime::RedDBRuntime;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
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

#[derive(Debug, Clone)]
struct PgPreparedStatement {
    sql: String,
    param_type_oids: Vec<u32>,
}

#[derive(Debug, Clone)]
struct PgPortal {
    sql: String,
    params: Vec<Value>,
    #[allow(dead_code)]
    result_format_codes: Vec<i16>,
    described_result: Option<crate::runtime::RuntimeQueryResult>,
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

    let mut prepared: HashMap<String, PgPreparedStatement> = HashMap::new();
    let mut portals: HashMap<String, PgPortal> = HashMap::new();

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
            FrontendMessage::Parse(msg) => {
                handle_parse(&mut stream, &mut prepared, msg).await?;
            }
            FrontendMessage::Bind(msg) => {
                handle_bind(&mut stream, &prepared, &mut portals, msg).await?;
            }
            FrontendMessage::Describe(msg) => {
                handle_describe(&mut stream, &runtime, &prepared, &mut portals, msg).await?;
            }
            FrontendMessage::Execute(msg) => {
                handle_execute(&mut stream, &runtime, &mut portals, msg).await?;
            }
            FrontendMessage::Close(msg) => {
                handle_close(&mut stream, &mut prepared, &mut portals, msg).await?;
            }
            FrontendMessage::Terminate => return Ok(()),
            FrontendMessage::Flush => {
                // Frames are written immediately; no additional marker is
                // needed. ReadyForQuery belongs to Sync, not Flush.
                continue;
            }
            FrontendMessage::Sync => {
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

async fn handle_parse<S>(
    stream: &mut S,
    prepared: &mut HashMap<String, PgPreparedStatement>,
    msg: super::protocol::ParseMessage,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    let inferred_param_type_oids = infer_pg_cast_param_type_oids(&msg.query);
    let sql = rewrite_pg_parameter_casts(&msg.query);
    let parsed_param_count = match crate::storage::query::modes::parse_multi(&sql) {
        Ok(parsed) => Some(
            crate::storage::query::user_params::scan_parameters(&parsed)
                .into_iter()
                .map(|param| param.index + 1)
                .max()
                .unwrap_or(0),
        ),
        Err(err) => {
            if pg_scalar_select_param_index(&sql).is_none() {
                send_error(stream, "42601", &err.to_string()).await?;
                return Ok(());
            }
            None
        }
    };
    let mut param_type_oids = msg.param_type_oids;
    if param_type_oids.is_empty() {
        let count = parsed_param_count
            .or_else(|| pg_scalar_select_param_index(&sql).map(|idx| idx + 1))
            .unwrap_or(0);
        param_type_oids.resize(count, PgOid::Unknown.as_u32());
    }
    for (idx, oid) in inferred_param_type_oids {
        if idx >= param_type_oids.len() {
            param_type_oids.resize(idx + 1, PgOid::Unknown.as_u32());
        }
        if param_type_oids[idx] == PgOid::Unknown.as_u32() {
            param_type_oids[idx] = oid;
        }
    }
    prepared.insert(
        msg.statement,
        PgPreparedStatement {
            sql,
            param_type_oids,
        },
    );
    write_frame(stream, &BackendMessage::ParseComplete).await
}

async fn handle_bind<S>(
    stream: &mut S,
    prepared: &HashMap<String, PgPreparedStatement>,
    portals: &mut HashMap<String, PgPortal>,
    msg: super::protocol::BindMessage,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    let Some(stmt) = prepared.get(&msg.statement) else {
        send_error(
            stream,
            "26000",
            &format!("prepared statement {:?} does not exist", msg.statement),
        )
        .await?;
        return Ok(());
    };
    let params = match bind_pg_params(stmt, &msg) {
        Ok(params) => params,
        Err(err) => {
            send_error(stream, "22023", &err).await?;
            return Ok(());
        }
    };
    portals.insert(
        msg.portal,
        PgPortal {
            sql: stmt.sql.clone(),
            params,
            result_format_codes: msg.result_format_codes,
            described_result: None,
        },
    );
    write_frame(stream, &BackendMessage::BindComplete).await
}

async fn handle_describe<S>(
    stream: &mut S,
    runtime: &RedDBRuntime,
    prepared: &HashMap<String, PgPreparedStatement>,
    portals: &mut HashMap<String, PgPortal>,
    msg: super::protocol::DescribeMessage,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    match msg.target {
        DescribeTarget::Statement => {
            let Some(stmt) = prepared.get(&msg.name) else {
                send_error(
                    stream,
                    "26000",
                    &format!("prepared statement {:?} does not exist", msg.name),
                )
                .await?;
                return Ok(());
            };
            write_frame(
                stream,
                &BackendMessage::ParameterDescription(stmt.param_type_oids.clone()),
            )
            .await?;
            write_frame(stream, &BackendMessage::NoData).await
        }
        DescribeTarget::Portal => {
            let Some(portal) = portals.get_mut(&msg.name) else {
                send_error(
                    stream,
                    "34000",
                    &format!("portal {:?} does not exist", msg.name),
                )
                .await?;
                return Ok(());
            };
            if is_row_returning_query(&portal.sql) {
                let result = match execute_pg_query_result(runtime, &portal.sql, &portal.params) {
                    Ok(result) => result,
                    Err(err) => {
                        let code = classify_sqlstate(&err);
                        send_error(stream, code, &err).await?;
                        return Ok(());
                    }
                };
                emit_row_description_for_result(stream, &result).await?;
                portal.described_result = Some(result);
                Ok(())
            } else {
                write_frame(stream, &BackendMessage::NoData).await
            }
        }
    }
}

async fn handle_execute<S>(
    stream: &mut S,
    runtime: &RedDBRuntime,
    portals: &mut HashMap<String, PgPortal>,
    msg: super::protocol::ExecuteMessage,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    let Some(portal) = portals.get_mut(&msg.portal) else {
        send_error(
            stream,
            "34000",
            &format!("portal {:?} does not exist", msg.portal),
        )
        .await?;
        return Ok(());
    };
    let _max_rows = msg.max_rows;
    let was_described = portal.described_result.is_some();
    let result = match portal.described_result.take() {
        Some(result) => Ok(result),
        None => execute_pg_query_result(runtime, &portal.sql, &portal.params),
    };
    match result {
        Ok(result) if was_described => {
            emit_success_result_without_row_description(stream, &result).await
        }
        Ok(result) => emit_success_result(stream, &result).await,
        Err(err) => {
            let code = classify_sqlstate(&err);
            send_error(stream, code, &err).await
        }
    }
}

async fn handle_close<S>(
    stream: &mut S,
    prepared: &mut HashMap<String, PgPreparedStatement>,
    portals: &mut HashMap<String, PgPortal>,
    msg: super::protocol::CloseMessage,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    match msg.target {
        DescribeTarget::Statement => {
            prepared.remove(&msg.name);
        }
        DescribeTarget::Portal => {
            portals.remove(&msg.name);
        }
    }
    write_frame(stream, &BackendMessage::CloseComplete).await
}

fn bind_pg_params(
    stmt: &PgPreparedStatement,
    msg: &super::protocol::BindMessage,
) -> Result<Vec<Value>, String> {
    if !matches!(msg.param_format_codes.len(), 0 | 1)
        && msg.param_format_codes.len() != msg.params.len()
    {
        return Err("Bind format count must be 0, 1, or match parameter count".to_string());
    }
    msg.params
        .iter()
        .enumerate()
        .map(|(idx, param)| {
            let oid = stmt
                .param_type_oids
                .get(idx)
                .copied()
                .unwrap_or(PgOid::Unknown.as_u32());
            let format_code = match msg.param_format_codes.as_slice() {
                [] => 0,
                [format] => *format,
                formats => formats[idx],
            };
            pg_param_to_value(oid, format_code, param.as_deref())
        })
        .collect()
}

fn execute_pg_query_result(
    runtime: &RedDBRuntime,
    sql: &str,
    params: &[Value],
) -> Result<crate::runtime::RuntimeQueryResult, String> {
    if let Some(result) = try_execute_pg_scalar_select(sql, params) {
        return Ok(result);
    }
    if params.is_empty() {
        return match translate_pg_catalog_query(runtime, sql) {
            Ok(Some(result)) => Ok(crate::runtime::RuntimeQueryResult {
                query: sql.to_string(),
                mode: crate::storage::query::modes::QueryMode::Sql,
                statement: "select",
                engine: "pg-catalog",
                result,
                affected_rows: 0,
                statement_type: "select",
            }),
            Ok(None) => runtime.execute_query(sql).map_err(|err| err.to_string()),
            Err(err) => Err(err.to_string()),
        };
    }

    let parsed = crate::storage::query::modes::parse_multi(sql).map_err(|err| err.to_string())?;
    let bound =
        crate::storage::query::user_params::bind(&parsed, params).map_err(|err| err.to_string())?;
    runtime
        .execute_query_expr(bound)
        .map_err(|err| err.to_string())
}

fn try_execute_pg_scalar_select(
    sql: &str,
    params: &[Value],
) -> Option<crate::runtime::RuntimeQueryResult> {
    let index = pg_scalar_select_param_index(sql)?;
    let value = params.get(index)?.clone();
    let mut result = UnifiedResult::with_columns(vec!["?column?".to_string()]);
    let mut record = UnifiedRecord::new();
    record.set("?column?", value);
    result.push(record);
    Some(crate::runtime::RuntimeQueryResult {
        query: sql.to_string(),
        mode: crate::storage::query::modes::QueryMode::Sql,
        statement: "select",
        engine: "pg-wire",
        result,
        affected_rows: 0,
        statement_type: "select",
    })
}

fn pg_scalar_select_param_index(sql: &str) -> Option<usize> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let body = lower.strip_prefix("select ")?;
    let param = if let Some(inner) = body.strip_prefix("cast(") {
        let end = inner.find(" as ")?;
        &inner[..end]
    } else {
        body.split_whitespace().next()?
    };
    let digits = param.strip_prefix('$')?;
    let n = digits.parse::<usize>().ok()?;
    n.checked_sub(1)
}

fn rewrite_pg_parameter_casts(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut cursor = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] != b'$' {
            pos += 1;
            continue;
        }
        let param_start = pos;
        pos += 1;
        let digits_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if digits_start == pos {
            continue;
        }
        if pos + 2 <= bytes.len() && &bytes[pos..pos + 2] == b"::" {
            let param_end = pos;
            pos += 2;
            let type_start = pos;
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric() || matches!(bytes[pos], b'_' | b'.'))
            {
                pos += 1;
            }
            if type_start != pos {
                out.push_str(&sql[cursor..param_start]);
                out.push_str(&sql[param_start..param_end]);
                cursor = pos;
                continue;
            }
        }
    }
    out.push_str(&sql[cursor..]);
    out
}

fn infer_pg_cast_param_type_oids(sql: &str) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let bytes = sql.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] != b'$' {
            pos += 1;
            continue;
        }
        pos += 1;
        let digits_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if digits_start == pos {
            continue;
        }
        let Some(param_index) = sql[digits_start..pos]
            .parse::<usize>()
            .ok()
            .and_then(|idx| idx.checked_sub(1))
        else {
            continue;
        };
        if pos + 2 > bytes.len() || &bytes[pos..pos + 2] != b"::" {
            continue;
        }
        pos += 2;
        let type_start = pos;
        while pos < bytes.len()
            && (bytes[pos].is_ascii_alphanumeric() || matches!(bytes[pos], b'_' | b'.'))
        {
            pos += 1;
        }
        if type_start == pos {
            continue;
        }
        if let Some(oid) = pg_cast_type_oid(&sql[type_start..pos]) {
            out.push((param_index, oid));
        }
    }
    out
}

fn pg_cast_type_oid(ty: &str) -> Option<u32> {
    let lower = ty.to_ascii_lowercase();
    let short = lower.rsplit('.').next().unwrap_or(lower.as_str());
    let oid = match short {
        "bool" | "boolean" => PgOid::Bool,
        "int2" | "smallint" => PgOid::Int2,
        "int" | "int4" | "integer" => PgOid::Int4,
        "int8" | "bigint" => PgOid::Int8,
        "float4" | "real" => PgOid::Float4,
        "float8" | "double" | "doubleprecision" => PgOid::Float8,
        "numeric" | "decimal" => PgOid::Numeric,
        "bytea" => PgOid::Bytea,
        "json" => PgOid::Json,
        "jsonb" => PgOid::Jsonb,
        "text" => PgOid::Text,
        "varchar" | "character varying" => PgOid::Varchar,
        "uuid" => PgOid::Uuid,
        "timestamp" => PgOid::Timestamp,
        "timestamptz" | "timestampz" => PgOid::TimestampTz,
        "vector" => PgOid::Vector,
        _ => return None,
    };
    Some(oid.as_u32())
}

fn is_row_returning_query(sql: &str) -> bool {
    let trimmed = sql.trim_start().to_ascii_lowercase();
    trimmed.starts_with("select")
        || trimmed.starts_with("with")
        || trimmed.starts_with("ask")
        || trimmed.starts_with("search")
        || trimmed.starts_with("vector")
        || trimmed.starts_with("hybrid")
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

    if let Some(tag) = pg_session_compat_command_tag(sql) {
        write_frame(stream, &BackendMessage::CommandComplete(tag.to_string())).await?;
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

fn pg_session_compat_command_tag(sql: &str) -> Option<&'static str> {
    let lower = sql.trim().trim_end_matches(';').to_ascii_lowercase();
    if lower.starts_with("set ") {
        return Some("SET");
    }
    None
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
    } else if result_returns_rows(result) {
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

async fn emit_success_result_without_row_description<S>(
    stream: &mut S,
    result: &crate::runtime::RuntimeQueryResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    if result.statement == "ask" {
        let row = ask_query_result_to_pg_wire_row(result)
            .ok_or_else(|| PgWireError::Protocol("ASK result missing row body".to_string()))?;
        write_frame(stream, &BackendMessage::DataRow(row.cells)).await?;
        write_frame(
            stream,
            &BackendMessage::CommandComplete("SELECT 1".to_string()),
        )
        .await?;
    } else if result_returns_rows(result) {
        emit_result_data_rows(stream, &result.result).await?;
        write_frame(
            stream,
            &BackendMessage::CommandComplete(format!("SELECT {}", result.result.records.len())),
        )
        .await?;
    } else {
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

async fn emit_row_description_for_result<S>(
    stream: &mut S,
    result: &crate::runtime::RuntimeQueryResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    if result.statement == "ask" {
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
        write_frame(stream, &BackendMessage::RowDescription(descriptors)).await
    } else if result_returns_rows(result) {
        emit_result_row_description(stream, &result.result).await
    } else {
        write_frame(stream, &BackendMessage::NoData).await
    }
}

fn result_returns_rows(result: &crate::runtime::RuntimeQueryResult) -> bool {
    result.statement_type == "select"
}

async fn emit_result_rows<S>(
    stream: &mut S,
    result: &crate::storage::query::unified::UnifiedResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    emit_result_row_description(stream, result).await?;
    emit_result_data_rows(stream, result).await
}

async fn emit_result_row_description<S>(
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

    write_frame(stream, &BackendMessage::RowDescription(descriptors)).await
}

async fn emit_result_data_rows<S>(
    stream: &mut S,
    result: &crate::storage::query::unified::UnifiedResult,
) -> Result<(), PgWireError>
where
    S: AsyncWrite + Unpin,
{
    let columns: Vec<String> = if !result.columns.is_empty() {
        result.columns.clone()
    } else if let Some(first) = result.records.first() {
        record_field_names(first)
    } else {
        Vec::new()
    };
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
    use crate::api::RedDBOptions;
    use crate::runtime::RuntimeQueryResult;
    use crate::storage::query::modes::QueryMode;
    use crate::storage::query::unified::UnifiedResult;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    #[tokio::test]
    async fn extended_parse_bind_execute_returns_rows() {
        let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
        let config = Arc::new(PgWireConfig::default());
        let (server_io, mut client_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            handle_connection(server_io, runtime, config).await.unwrap();
        });

        write_startup(&mut client_io).await;
        read_until_ready(&mut client_io).await;

        write_frontend_frame(
            &mut client_io,
            b'P',
            parse_body("", "SELECT $1::int", &[PgOid::Int4.as_u32()]),
        )
        .await;
        write_frontend_frame(
            &mut client_io,
            b'B',
            bind_body("", "", &[0], &[Some(b"42".as_slice())], &[]),
        )
        .await;
        write_frontend_frame(&mut client_io, b'D', describe_body(b'P', "")).await;
        write_frontend_frame(&mut client_io, b'E', execute_body("", 0)).await;
        write_frontend_frame(&mut client_io, b'S', Vec::new()).await;

        let frames = read_until_ready(&mut client_io).await;
        assert_eq!(
            frames.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            b"12TDCZ"
        );
        let columns = decode_row_description(&frames[2].1);
        assert_eq!(columns.len(), 1);
        let cells = decode_data_row(&frames[3].1);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].as_deref(), Some(b"42".as_slice()));
        assert_eq!(decode_command_complete(&frames[4].1), "SELECT 1");

        write_frontend_frame(&mut client_io, b'X', Vec::new()).await;
        server.await.unwrap();
    }

    #[test]
    fn infer_pg_cast_param_type_oids_from_parameter_casts() {
        assert_eq!(
            infer_pg_cast_param_type_oids("INSERT INTO t (id, name) VALUES ($1::int, $2::text)"),
            vec![(0, PgOid::Int4.as_u32()), (1, PgOid::Text.as_u32())]
        );
        assert_eq!(
            infer_pg_cast_param_type_oids("SEARCH SIMILAR [1.0] COLLECTION v LIMIT $1::int8"),
            vec![(0, PgOid::Int8.as_u32())]
        );
    }

    #[test]
    fn pg_session_compat_accepts_driver_setup_set_commands() {
        assert_eq!(
            pg_session_compat_command_tag("SET extra_float_digits = 3"),
            Some("SET")
        );
        assert_eq!(
            pg_session_compat_command_tag("SET application_name = 'pgjdbc'"),
            Some("SET")
        );
        assert_eq!(pg_session_compat_command_tag("SELECT 1"), None);
    }

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

    async fn write_startup<W: AsyncWrite + Unpin>(stream: &mut W) {
        let mut payload = Vec::new();
        payload.extend_from_slice(&crate::wire::postgres::protocol::PG_PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0reddb\0");
        payload.push(0);
        let len = (payload.len() + 4) as u32;
        stream.write_all(&len.to_be_bytes()).await.unwrap();
        stream.write_all(&payload).await.unwrap();
    }

    async fn write_frontend_frame<W: AsyncWrite + Unpin>(
        stream: &mut W,
        tag: u8,
        payload: Vec<u8>,
    ) {
        stream.write_all(&[tag]).await.unwrap();
        stream
            .write_all(&((payload.len() + 4) as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(&payload).await.unwrap();
    }

    async fn read_backend_frame<R: AsyncRead + Unpin>(stream: &mut R) -> (u8, Vec<u8>) {
        let mut tag = [0u8; 1];
        stream.read_exact(&mut tag).await.unwrap();
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).await.unwrap();
        let len = u32::from_be_bytes(len) as usize;
        let mut body = vec![0u8; len - 4];
        stream.read_exact(&mut body).await.unwrap();
        (tag[0], body)
    }

    async fn read_until_ready<R: AsyncRead + Unpin>(stream: &mut R) -> Vec<(u8, Vec<u8>)> {
        let mut frames = Vec::new();
        loop {
            let frame = read_backend_frame(stream).await;
            let done = frame.0 == b'Z';
            frames.push(frame);
            if done {
                return frames;
            }
        }
    }

    fn parse_body(statement: &str, query: &str, oids: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        push_pg_cstring(&mut out, statement);
        push_pg_cstring(&mut out, query);
        out.extend_from_slice(&(oids.len() as i16).to_be_bytes());
        for oid in oids {
            out.extend_from_slice(&oid.to_be_bytes());
        }
        out
    }

    fn bind_body(
        portal: &str,
        statement: &str,
        formats: &[i16],
        params: &[Option<&[u8]>],
        result_formats: &[i16],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_pg_cstring(&mut out, portal);
        push_pg_cstring(&mut out, statement);
        out.extend_from_slice(&(formats.len() as i16).to_be_bytes());
        for format in formats {
            out.extend_from_slice(&format.to_be_bytes());
        }
        out.extend_from_slice(&(params.len() as i16).to_be_bytes());
        for param in params {
            match param {
                Some(bytes) => {
                    out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                    out.extend_from_slice(bytes);
                }
                None => out.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        out.extend_from_slice(&(result_formats.len() as i16).to_be_bytes());
        for format in result_formats {
            out.extend_from_slice(&format.to_be_bytes());
        }
        out
    }

    fn describe_body(target: u8, name: &str) -> Vec<u8> {
        let mut out = vec![target];
        push_pg_cstring(&mut out, name);
        out
    }

    fn execute_body(portal: &str, max_rows: u32) -> Vec<u8> {
        let mut out = Vec::new();
        push_pg_cstring(&mut out, portal);
        out.extend_from_slice(&max_rows.to_be_bytes());
        out
    }

    fn push_pg_cstring(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
}
