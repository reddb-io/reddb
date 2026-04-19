/// RedDB Wire Protocol TCP Listener (plaintext + TLS)
///
/// Accepts TCP connections and processes binary wire protocol messages.
/// Each connection is handled in its own tokio task.
/// Supports both plaintext TCP and TLS-encrypted connections.
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use super::protocol::*;
use crate::application::ports::RuntimeEntityPort;
use crate::runtime::RedDBRuntime;
use crate::storage::query::sql_lowering::effective_table_filter;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, EntityId};

/// Start the wire protocol TCP listener (plaintext).
pub async fn start_wire_listener(
    bind_addr: &str,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(transport = "wire", bind = %bind_addr, "listener online");
    start_wire_listener_on(listener, runtime).await
}

pub async fn start_wire_listener_on(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let rt = runtime.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, rt).await {
                tracing::warn!(transport = "wire", peer = %peer_str, err = %e, "connection failed");
            }
        });
    }
}

/// Start the wire protocol listener on a Unix domain socket (Phase 1.7 PG parity).
///
/// Accepts connections from `unix://path` URLs or plain filesystem paths.
/// Existing socket files are removed before bind (parallel to PG's behaviour).
/// Connection handling reuses `handle_connection`, which is generic over any
/// `AsyncRead + AsyncWrite` stream.
#[cfg(unix)]
pub async fn start_wire_unix_listener(
    socket_path: &str,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::net::UnixListener;

    // Normalise: strip `unix://` prefix if caller passed a URL.
    let path: &str = socket_path.strip_prefix("unix://").unwrap_or(socket_path);

    // Remove stale socket file so bind() doesn't fail with EADDRINUSE.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    tracing::info!(transport = "wire-unix", bind = %path, "listener online");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let rt = runtime.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, rt).await {
                tracing::warn!(transport = "wire-unix", err = %e, "connection failed");
            }
        });
    }
}

/// Start the wire protocol TCP listener with TLS encryption.
pub async fn start_wire_tls_listener(
    bind_addr: &str,
    runtime: Arc<RedDBRuntime>,
    tls_config: &super::tls::WireTlsConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let acceptor = super::tls::build_tls_acceptor(tls_config)?;
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(transport = "wire+tls", bind = %bind_addr, "listener online");
    start_wire_tls_listener_on(listener, runtime, acceptor).await
}

async fn start_wire_tls_listener_on(
    listener: TcpListener,
    runtime: Arc<RedDBRuntime>,
    acceptor: tokio_rustls::TlsAcceptor,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (tcp_stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let rt = runtime.clone();
        let peer_str = peer.to_string();
        tokio::spawn(async move {
            match acceptor.accept(tcp_stream).await {
                Ok(tls_stream) => {
                    if let Err(e) = handle_connection(tls_stream, rt).await {
                        tracing::warn!(transport = "wire+tls", peer = %peer_str, err = %e, "connection failed");
                    }
                }
                Err(e) => tracing::warn!(
                    transport = "wire+tls",
                    peer = %peer_str,
                    err = %e,
                    "TLS handshake failed"
                ),
            }
        });
    }
}

/// Handle a connection (works for both plain TCP and TLS streams).
async fn handle_connection<S>(
    mut stream: S,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut header_buf = [0u8; 5]; // 4 bytes len + 1 byte msg_type

    loop {
        // Read frame header
        match stream.read_exact(&mut header_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }

        let total_len =
            u32::from_le_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]])
                as usize;
        let msg_type = header_buf[4];
        let payload_len = total_len.saturating_sub(1);

        // Read payload
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            stream.read_exact(&mut payload).await?;
        }

        // Process message
        let response = match msg_type {
            MSG_QUERY => handle_query(&runtime, &payload),
            MSG_QUERY_BINARY => handle_query_binary(&runtime, &payload),
            MSG_BULK_INSERT => handle_bulk_insert(&runtime, &payload),
            MSG_BULK_INSERT_BINARY => handle_bulk_insert_binary(&runtime, &payload),
            _ => {
                let mut resp = Vec::new();
                let err = b"unknown message type";
                write_frame_header(&mut resp, MSG_ERROR, err.len() as u32);
                resp.extend_from_slice(err);
                resp
            }
        };

        // Send response
        stream.write_all(&response).await?;
    }
}

fn handle_query(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let sql = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return make_error(b"invalid UTF-8 in query"),
    };

    match runtime.execute_query(sql) {
        Ok(result) => {
            // Fast path: if pre_serialized_json available, send it as text
            // (avoids gRPC/protobuf overhead while reusing existing JSON turbo path)
            if let Some(ref json) = result.result.pre_serialized_json {
                let json_bytes = json.as_bytes();
                let mut resp = Vec::with_capacity(5 + json_bytes.len());
                write_frame_header(&mut resp, MSG_RESULT, json_bytes.len() as u32);
                resp.extend_from_slice(json_bytes);
                return resp;
            }
            encode_result(&result)
        }
        Err(e) => make_error(e.to_string().as_bytes()),
    }
}

/// Handle query with BINARY result encoding — zero JSON.
/// Parses SQL, scans entities, encodes directly to wire binary.
fn handle_query_binary(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let sql = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return make_error(b"invalid UTF-8"),
    };

    // Parse SQL to get table query
    let expr = match crate::storage::query::modes::parse_multi(sql) {
        Ok(e) => e,
        Err(e) => return make_error(format!("parse: {e}").as_bytes()),
    };

    let table_query = match &expr {
        crate::storage::query::ast::QueryExpr::Table(tq) => tq,
        // For non-table queries (UPDATE/DELETE etc), fall back to JSON path
        _ => return handle_query(runtime, payload),
    };

    let db = runtime.db();
    let store = db.store();

    // Entity_id point lookup — single entity binary
    let effective_filter = effective_table_filter(table_query);

    if let Some(entity_id) =
        crate::runtime::query_exec::extract_entity_id_from_filter(&effective_filter)
    {
        return match store.get(&table_query.table, EntityId::new(entity_id)) {
            Some(entity) => encode_entity_binary(&entity),
            None => encode_empty_result(),
        };
    }

    // Filtered scan — encode matching entities to binary directly
    let manager = match store.get_collection(&table_query.table) {
        Some(m) => m,
        None => return make_error(b"collection not found"),
    };

    let filter = effective_filter.as_ref();
    let table_name = table_query.table.as_str();
    let table_alias = table_query.alias.as_deref().unwrap_or(table_name);
    let limit = table_query.limit.unwrap_or(10000) as usize;

    // First entity determines column schema
    let mut col_names: Option<Vec<String>> = None;
    let mut row_bufs: Vec<Vec<u8>> = Vec::new();
    let mut count = 0usize;

    manager.for_each_entity(|entity| {
        if count >= limit {
            return false;
        }
        if !entity.data.is_row() {
            return true;
        }

        if let Some(f) = filter {
            if !crate::runtime::query_exec::evaluate_entity_filter(
                entity,
                f,
                table_name,
                table_alias,
            ) {
                return true;
            }
        }

        // Initialize columns from first entity
        if col_names.is_none() {
            let mut cols = vec![
                "red_entity_id".into(),
                "created_at".into(),
                "updated_at".into(),
            ];
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    cols.extend(named.keys().cloned());
                }
            }
            col_names = Some(cols);
        }

        let Some(cols) = col_names.as_ref() else {
            return true;
        };
        let mut row = Vec::with_capacity(cols.len() * 10);
        for col in cols {
            let val = match col.as_str() {
                "red_entity_id" => Value::UnsignedInteger(entity.id.raw()),
                "created_at" => Value::UnsignedInteger(entity.created_at),
                "updated_at" => Value::UnsignedInteger(entity.updated_at),
                other => {
                    if let EntityData::Row(ref r) = entity.data {
                        r.named
                            .as_ref()
                            .and_then(|n| n.get(other))
                            .cloned()
                            .unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    }
                }
            };
            encode_value(&mut row, &val);
        }
        row_bufs.push(row);
        count += 1;
        true
    });

    // Build response frame
    let cols = col_names.unwrap_or_default();
    let mut body = Vec::with_capacity(256 + count * 64);
    body.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for col in &cols {
        encode_column_name(&mut body, col);
    }
    body.extend_from_slice(&(count as u32).to_le_bytes());
    for row in &row_bufs {
        body.extend_from_slice(row);
    }

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    resp
}

fn encode_entity_binary(entity: &crate::storage::unified::UnifiedEntity) -> Vec<u8> {
    let mut cols: Vec<String> = vec![
        "red_entity_id".into(),
        "created_at".into(),
        "updated_at".into(),
    ];
    if let EntityData::Row(ref row) = entity.data {
        if let Some(ref named) = row.named {
            cols.extend(named.keys().cloned());
        }
    }

    let mut body = Vec::with_capacity(256);
    body.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for col in &cols {
        encode_column_name(&mut body, col);
    }
    body.extend_from_slice(&1u32.to_le_bytes());
    for col in &cols {
        let val = match col.as_str() {
            "red_entity_id" => Value::UnsignedInteger(entity.id.raw()),
            "created_at" => Value::UnsignedInteger(entity.created_at),
            "updated_at" => Value::UnsignedInteger(entity.updated_at),
            other => {
                if let EntityData::Row(ref r) = entity.data {
                    r.named
                        .as_ref()
                        .and_then(|n| n.get(other))
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }
        };
        encode_value(&mut body, &val);
    }

    let mut resp = Vec::with_capacity(5 + body.len());
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    resp
}

fn encode_empty_result() -> Vec<u8> {
    let body = [0u8, 0, 0, 0, 0, 0]; // ncols=0, nrows=0
    let mut resp = Vec::with_capacity(11);
    write_frame_header(&mut resp, MSG_RESULT, body.len() as u32);
    resp.extend_from_slice(&body);
    resp
}

fn handle_bulk_insert(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let mut pos = 0;

    // Collection name
    if payload.len() < 2 {
        return make_error(b"bulk insert: payload too short");
    }
    let coll_len = match read_u16(payload, &mut pos, "bulk insert: missing collection length") {
        Ok(len) => len as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let collection = match read_string(
        payload,
        &mut pos,
        coll_len,
        "bulk insert: truncated collection name",
        "bulk insert: invalid collection name",
    ) {
        Ok(s) => s,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    // Number of rows
    let nrows = match read_u32(payload, &mut pos, "bulk insert: missing row count") {
        Ok(rows) => rows as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    // Parse JSON payloads
    let mut json_payloads = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let json_len = match read_u32(payload, &mut pos, "bulk insert: missing JSON length") {
            Ok(len) => len as usize,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        let json_str = match read_string(
            payload,
            &mut pos,
            json_len,
            "bulk insert: truncated JSON payload",
            "bulk insert: invalid JSON payload",
        ) {
            Ok(s) => s,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        json_payloads.push(json_str);
    }

    let mut rows = Vec::with_capacity(nrows);
    for json_str in &json_payloads {
        let parsed: crate::json::Value = match crate::json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => return make_error(format!("JSON parse: {e}").as_bytes()),
        };
        let input = match crate::application::entity_payload::parse_create_row_input(
            collection.clone(),
            &parsed,
        ) {
            Ok(input) => input,
            Err(err) => return make_error(format!("bulk insert: {err}").as_bytes()),
        };
        rows.push(input);
    }

    match runtime.create_rows_batch(crate::application::CreateRowsBatchInput { collection, rows }) {
        Ok(outputs) => {
            let count = outputs.len() as u64;
            let mut resp = Vec::with_capacity(13);
            write_frame_header(&mut resp, MSG_BULK_OK, 8);
            resp.extend_from_slice(&count.to_le_bytes());
            resp
        }
        Err(e) => make_error(format!("bulk insert: {e}").as_bytes()),
    }
}

fn encode_result(result: &crate::runtime::RuntimeQueryResult) -> Vec<u8> {
    // For pre-serialized JSON results, we still need to encode as binary rows
    // But the entity data is available via the query result records
    let records = &result.result.records;

    // If pre_serialized_json is set but records is empty, we need entity-level access
    // For now, encode available records as binary
    let mut payload = Vec::with_capacity(256 + records.len() * 128);

    // Collect column names from first record (or from result.columns)
    let columns: Vec<String> = if !result.result.columns.is_empty() {
        result.result.columns.clone()
    } else if let Some(first) = records.first() {
        first.values.keys().map(|k| k.to_string()).collect()
    } else {
        Vec::new()
    };

    // ncols
    payload.extend_from_slice(&(columns.len() as u16).to_le_bytes());

    // Column names
    for col in &columns {
        encode_column_name(&mut payload, col);
    }

    // nrows
    payload.extend_from_slice(&(records.len() as u32).to_le_bytes());

    // Row data
    for record in records {
        for col in &columns {
            let value = record.values.get(col.as_str()).unwrap_or(&Value::Null);
            encode_value(&mut payload, value);
        }
    }

    // Wrap in frame
    let mut resp = Vec::with_capacity(5 + payload.len());
    write_frame_header(&mut resp, MSG_RESULT, payload.len() as u32);
    resp.extend_from_slice(&payload);
    resp
}

/// Binary bulk insert — zero JSON parsing. Values come as typed wire bytes.
/// Format: [coll_len:u16][coll][ncols:u16][col_names...][nrows:u32][row_values...]
fn handle_bulk_insert_binary(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let mut pos = 0;

    if payload.len() < 6 {
        return make_error(b"binary bulk: payload too short");
    }

    // Collection name
    let coll_len = match read_u16(payload, &mut pos, "binary bulk: missing collection length") {
        Ok(len) => len as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let collection = match read_string(
        payload,
        &mut pos,
        coll_len,
        "binary bulk: truncated collection name",
        "binary bulk: invalid collection name",
    ) {
        Ok(s) => s,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    // Column names
    let ncols = match read_u16(payload, &mut pos, "binary bulk: missing column count") {
        Ok(cols) => cols as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let mut col_names = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = match read_u16(payload, &mut pos, "binary bulk: missing column name length")
        {
            Ok(len) => len as usize,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        let name = match read_string(
            payload,
            &mut pos,
            name_len,
            "binary bulk: truncated column name",
            "binary bulk: invalid column name",
        ) {
            Ok(s) => s,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        col_names.push(name);
    }

    // Number of rows
    let nrows = match read_u32(payload, &mut pos, "binary bulk: missing row count") {
        Ok(rows) => rows as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    let mut rows = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let mut fields = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let value = match try_decode_value(payload, &mut pos) {
                Ok(value) => value,
                Err(err) => return make_error(format!("binary bulk: {err}").as_bytes()),
            };
            let field_name = col_names
                .get(fields.len())
                .cloned()
                .unwrap_or_else(|| format!("col_{}", fields.len()));
            fields.push((field_name, value));
        }
        rows.push(crate::application::CreateRowInput {
            collection: collection.clone(),
            fields,
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        });
    }

    match runtime.create_rows_batch(crate::application::CreateRowsBatchInput { collection, rows }) {
        Ok(outputs) => {
            let count = outputs.len() as u64;
            let mut resp = Vec::with_capacity(13);
            write_frame_header(&mut resp, MSG_BULK_OK, 8);
            resp.extend_from_slice(&count.to_le_bytes());
            resp
        }
        Err(e) => make_error(format!("bulk insert: {e}").as_bytes()),
    }
}

fn make_error(msg: &[u8]) -> Vec<u8> {
    let mut resp = Vec::with_capacity(5 + msg.len());
    write_frame_header(&mut resp, MSG_ERROR, msg.len() as u32);
    resp.extend_from_slice(msg);
    resp
}

fn read_u16(payload: &[u8], pos: &mut usize, err: &'static str) -> Result<u16, &'static str> {
    let bytes = read_bytes(payload, pos, 2, err)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], pos: &mut usize, err: &'static str) -> Result<u32, &'static str> {
    let bytes = read_bytes(payload, pos, 4, err)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_string(
    payload: &[u8],
    pos: &mut usize,
    len: usize,
    truncated_err: &'static str,
    utf8_err: &'static str,
) -> Result<String, &'static str> {
    let bytes = read_bytes(payload, pos, len, truncated_err)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| utf8_err)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: &'static str,
) -> Result<&'a [u8], &'static str> {
    let end = pos.saturating_add(len);
    if end > payload.len() {
        return Err(err);
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_runtime() -> RedDBRuntime {
        RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
    }

    fn decode_error_message(response: &[u8]) -> String {
        assert_eq!(response.get(4), Some(&MSG_ERROR));
        String::from_utf8(response[5..].to_vec()).expect("wire error should be utf-8")
    }

    #[test]
    fn bulk_insert_rejects_truncated_json_payload() {
        let runtime = create_runtime();
        let payload = vec![
            1, 0, b't', // collection "t"
            1, 0, 0, 0, // one row
            5, 0, 0, 0, b'{', b'}', // declares len 5 but only 2 bytes
        ];

        let response = handle_bulk_insert(&runtime, &payload);
        assert_eq!(
            decode_error_message(&response),
            "bulk insert: truncated JSON payload"
        );
    }

    #[test]
    fn binary_bulk_rejects_invalid_collection_name() {
        let runtime = create_runtime();
        let payload = vec![
            1, 0, 0xff, // invalid utf-8 collection
            0, 0, // ncols
            0, 0, 0, 0, // nrows
        ];

        let response = handle_bulk_insert_binary(&runtime, &payload);
        assert_eq!(
            decode_error_message(&response),
            "binary bulk: invalid collection name"
        );
    }

    #[test]
    fn binary_bulk_rejects_truncated_value_payload() {
        let runtime = create_runtime();
        let payload = vec![
            1, 0, b't', // collection "t"
            1, 0, // ncols = 1
            1, 0, b'x', // column "x"
            1, 0, 0, 0,       // nrows = 1
            VAL_I64, // truncated i64, missing 8-byte payload
        ];

        let response = handle_bulk_insert_binary(&runtime, &payload);
        assert_eq!(
            decode_error_message(&response),
            "binary bulk: truncated i64 value"
        );
    }
}
