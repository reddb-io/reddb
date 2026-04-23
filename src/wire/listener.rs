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

    // Streaming-bulk session state. Populated by MSG_BULK_STREAM_START,
    // appended to by each MSG_BULK_STREAM_ROWS, flushed and cleared
    // by MSG_BULK_STREAM_COMMIT. One session per connection at a
    // time — a second START before COMMIT returns an error.
    let mut stream_session: Option<BulkStreamSession> = None;

    // Per-connection timing aggregation. Flushed every 500 requests
    // when REDDB_WIRE_TIMING=1 is set (bench mode only). Zero overhead
    // when the env flag is absent — we don't even sample.
    let trace = matches!(
        std::env::var("REDDB_WIRE_TIMING").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    );
    let mut trace_count: u64 = 0;
    let mut trace_read_ns: u64 = 0;
    let mut trace_process_ns: u64 = 0;
    let mut trace_write_ns: u64 = 0;

    loop {
        let t_read = if trace {
            Some(std::time::Instant::now())
        } else {
            None
        };

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

        if let Some(t) = t_read {
            trace_read_ns += t.elapsed().as_nanos() as u64;
        }

        let t_proc = if trace {
            Some(std::time::Instant::now())
        } else {
            None
        };

        // Process message
        let response = match msg_type {
            MSG_QUERY => handle_query(&runtime, &payload),
            MSG_QUERY_BINARY => handle_query_binary(&runtime, &payload),
            MSG_BULK_INSERT => handle_bulk_insert(&runtime, &payload),
            MSG_BULK_INSERT_BINARY => handle_bulk_insert_binary(&runtime, &payload),
            MSG_BULK_INSERT_PREVALIDATED => {
                handle_bulk_insert_binary_prevalidated(&runtime, &payload)
            }
            MSG_BULK_STREAM_START => handle_stream_start(&payload, &mut stream_session),
            MSG_BULK_STREAM_ROWS => {
                // STREAM_ROWS is the hot path — if the append succeeds we
                // send NO response and the client pipelines the next
                // frame. Only on error does the server reply (MSG_ERROR).
                // Handler returns an empty Vec to signal "no response".
                handle_stream_rows(&payload, &mut stream_session)
            }
            MSG_BULK_STREAM_COMMIT => handle_stream_commit(&runtime, &mut stream_session),
            _ => {
                let mut resp = Vec::new();
                let err = b"unknown message type";
                write_frame_header(&mut resp, MSG_ERROR, err.len() as u32);
                resp.extend_from_slice(err);
                resp
            }
        };

        if let Some(t) = t_proc {
            trace_process_ns += t.elapsed().as_nanos() as u64;
        }

        let t_write = if trace {
            Some(std::time::Instant::now())
        } else {
            None
        };

        // Send response — empty vec means "no response" (used by the
        // streaming-bulk hot path so clients pipeline next frame).
        if !response.is_empty() {
            stream.write_all(&response).await?;
        }

        if let Some(t) = t_write {
            trace_write_ns += t.elapsed().as_nanos() as u64;
        }

        if trace {
            trace_count += 1;
            if trace_count % 500 == 0 {
                eprintln!(
                    "[wire-timing] requests={} avg_read_us={} avg_process_us={} avg_write_us={}",
                    trace_count,
                    trace_read_ns / trace_count / 1000,
                    trace_process_ns / trace_count / 1000,
                    trace_write_ns / trace_count / 1000,
                );
            }
        }
    }
}

fn handle_query(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let sql = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return make_error(b"invalid UTF-8 in query"),
    };

    // Zero-copy fast path for simple indexed SELECT. Returns None
    // unchanged when the shape / filter / index don't qualify, so we
    // fall through to the standard executor without semantic drift.
    if let Some(resp) = super::query_direct::try_handle_query_binary_direct(runtime, sql) {
        return resp;
    }

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

/// Handle a MSG_QUERY_BINARY request.
///
/// Historically this path had its own ad-hoc scan+encode loop that
/// duplicated the runtime's filter/projection/index logic and only
/// covered the `named` storage layout — every WHERE clause on a
/// columnar (bulk-inserted) row returned 0 rows. Rather than keep
/// two parallel SELECT implementations in sync, delegate to the
/// same path `handle_query` uses: `runtime.execute_query` applies
/// every optimiser + index path we have, and `encode_result` emits
/// the identical `[ncols][cols][nrows][(tag+bytes)*]` binary format
/// on the wire. The only observable difference between MSG_QUERY
/// and MSG_QUERY_BINARY is the client-side intent marker — useful
/// for routing/telemetry, but no longer a functional divergence.
fn handle_query_binary(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    handle_query(runtime, payload)
}

#[allow(dead_code)]
fn _unused_binary_scan_loop_kept_for_reference(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    // Old custom-scan implementation retained as a reference for the
    // filter-compiler + binary-encoder patterns; not linked into the
    // wire dispatch.
    let sql = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return make_error(b"invalid UTF-8"),
    };

    let expr = match crate::storage::query::modes::parse_multi(sql) {
        Ok(e) => e,
        Err(e) => return make_error(format!("parse: {e}").as_bytes()),
    };

    let table_query = match &expr {
        crate::storage::query::ast::QueryExpr::Table(tq) => tq,
        _ => return handle_query(runtime, payload),
    };

    let db = runtime.db();
    let store = db.store();

    let effective_filter = effective_table_filter(table_query);

    if let Some(entity_id) =
        crate::runtime::query_exec::extract_entity_id_from_filter(&effective_filter)
    {
        return match store.get(&table_query.table, EntityId::new(entity_id)) {
            Some(entity) => encode_entity_binary(&entity),
            None => encode_empty_result(),
        };
    }

    let manager = match store.get_collection(&table_query.table) {
        Some(m) => m,
        None => return make_error(b"collection not found"),
    };

    let filter = effective_filter.as_ref();
    let table_name = table_query.table.as_str();
    let table_alias = table_query.alias.as_deref().unwrap_or(table_name);
    let limit = table_query.limit.unwrap_or(10000) as usize;

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

        if col_names.is_none() {
            let mut cols = vec![
                "red_entity_id".into(),
                "created_at".into(),
                "updated_at".into(),
            ];
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    cols.extend(named.keys().cloned());
                } else if let Some(ref schema) = row.schema {
                    cols.extend(schema.iter().cloned());
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
                        r.get_field(other).cloned().unwrap_or(Value::Null)
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

/// Per-column resolver built once per `encode_result` call. The row
/// loop then dispatches via `&[ColumnResolver]` with one cheap
/// lookup per cell instead of re-scanning the columnar schema for
/// every (row, column) pair.
enum ColumnResolver {
    /// Direct index into every record's `columnar.values[]`. Valid
    /// when the scan result shares a single `Arc<Vec<Arc<str>>>`
    /// schema across records.
    ColumnarIdx(usize),
    /// HashMap fallback — records built via `set*` or mutated after
    /// columnar construction.
    HashMapKey(Arc<str>),
}

thread_local! {
    /// Scratch buffer reused across `encode_result` calls on the same
    /// task. Avoids a ~500 kB alloc + dealloc for every SELECT scan.
    static ENCODE_SCRATCH: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn encode_result(result: &crate::runtime::RuntimeQueryResult) -> Vec<u8> {
    let records = &result.result.records;

    // Column name list. Prefer the executor's projected columns when
    // populated; fall back to the first record's schema so scan rows
    // with an empty `values` HashMap still surface their fields.
    let columns: Vec<Arc<str>> = if !result.result.columns.is_empty() {
        result
            .result
            .columns
            .iter()
            .map(|s| Arc::from(s.as_str()))
            .collect()
    } else if let Some(first) = records.first() {
        first.column_names()
    } else {
        Vec::new()
    };

    // Pre-resolve each column against the first record's columnar
    // schema. For scan outputs every record shares the same `Arc`
    // pointer, so the resolver list is valid for every row; per-row
    // encoding becomes an O(1) `values[idx]` per cell.
    //
    // Records built via `set*` (HashMap path) still resolve via the
    // `HashMapKey` variant with a single HashMap lookup per cell —
    // no worse than before the optimisation.
    let first_schema_ptr = records
        .first()
        .and_then(|r| r.columnar_schema())
        .map(|s| Arc::as_ptr(s) as usize);
    let first_columnar = records.first().and_then(|r| r.columnar());
    let resolvers: Vec<ColumnResolver> = columns
        .iter()
        .map(|col| {
            match first_columnar
                .and_then(|c| c.schema.iter().rposition(|k| k.as_ref() == col.as_ref()))
            {
                Some(idx) => ColumnResolver::ColumnarIdx(idx),
                None => ColumnResolver::HashMapKey(Arc::clone(col)),
            }
        })
        .collect();

    // Scratch-buffer reuse. Size hint is a rough estimate (~130 B
    // per row for typical 9-column integer+text rows); actual growth
    // amortises over sustained query streams on the same worker.
    let payload = ENCODE_SCRATCH.with(|cell| {
        let mut buf = cell.borrow_mut();
        buf.clear();
        buf.reserve(256 + records.len() * 128);

        buf.extend_from_slice(&(columns.len() as u16).to_le_bytes());
        for col in &columns {
            encode_column_name(&mut buf, col);
        }
        buf.extend_from_slice(&(records.len() as u32).to_le_bytes());

        for record in records {
            // When the schema identity matches the first record's
            // pointer, `ColumnarIdx(i)` is safe — we just reach
            // into `record.columnar().unwrap().values[i]`. Otherwise
            // fall back to the record-level `get` which covers
            // heterogeneous schemas (happens when the executor
            // mixes result shapes, e.g. a projection over UNION).
            let same_schema = record.columnar_schema().map(|s| Arc::as_ptr(s) as usize)
                == first_schema_ptr
                && first_schema_ptr.is_some();
            if same_schema {
                let col = record.columnar().expect("checked above");
                for resolver in &resolvers {
                    let val = match resolver {
                        ColumnResolver::ColumnarIdx(i) => {
                            col.values.get(*i).unwrap_or(&Value::Null)
                        }
                        ColumnResolver::HashMapKey(k) => {
                            record.get(k.as_ref()).unwrap_or(&Value::Null)
                        }
                    };
                    encode_value(&mut buf, val);
                }
            } else {
                // Heterogeneous-schema fallback. Walk columns + the
                // already-built resolver list in lockstep so each
                // cell pays one `record.get` (same as pre-Phase-B
                // behaviour) — no O(N²) pointer search.
                for (col_name, resolver) in columns.iter().zip(resolvers.iter()) {
                    let key: &str = match resolver {
                        ColumnResolver::ColumnarIdx(_) => col_name.as_ref(),
                        ColumnResolver::HashMapKey(k) => k.as_ref(),
                    };
                    let val = record.get(key).unwrap_or(&Value::Null);
                    encode_value(&mut buf, val);
                }
            }
        }

        buf.clone()
    });

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

/// `MSG_BULK_INSERT_PREVALIDATED` handler — same wire format as
/// `MSG_BULK_INSERT_BINARY`, but routes through the port's
/// pre-validated path which skips per-row contract + uniqueness
/// checks. Used by typed-bench-style workloads where the client
/// already validated types before sending.
fn handle_bulk_insert_binary_prevalidated(runtime: &RedDBRuntime, payload: &[u8]) -> Vec<u8> {
    let mut pos = 0;

    if payload.len() < 6 {
        return make_error(b"binary bulk prevalidated: payload too short");
    }

    let coll_len = match read_u16(payload, &mut pos, "prevalidated: missing collection length") {
        Ok(len) => len as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let collection = match read_string(
        payload,
        &mut pos,
        coll_len,
        "prevalidated: truncated collection name",
        "prevalidated: invalid collection name",
    ) {
        Ok(s) => s,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    let ncols = match read_u16(payload, &mut pos, "prevalidated: missing column count") {
        Ok(cols) => cols as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let mut col_names = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = match read_u16(
            payload,
            &mut pos,
            "prevalidated: missing column name length",
        ) {
            Ok(len) => len as usize,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        let name = match read_string(
            payload,
            &mut pos,
            name_len,
            "prevalidated: truncated column name",
            "prevalidated: invalid column name",
        ) {
            Ok(s) => s,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        col_names.push(name);
    }

    let nrows = match read_u32(payload, &mut pos, "prevalidated: missing row count") {
        Ok(rows) => rows as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    // Columnar decode: one `Vec<Value>` per row, one shared
    // `Arc<Vec<String>>` for the schema. Skips the N × ncols
    // String clones + HashMap builds the tuple-based path did.
    let schema = std::sync::Arc::new(col_names);
    let mut rows: Vec<Vec<crate::storage::schema::Value>> = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let mut values = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let value = match try_decode_value(payload, &mut pos) {
                Ok(v) => v,
                Err(err) => return make_error(format!("prevalidated: {err}").as_bytes()),
            };
            values.push(value);
        }
        rows.push(values);
    }

    match runtime.create_rows_batch_prevalidated_columnar(collection, schema, rows) {
        Ok(count) => {
            let mut resp = Vec::with_capacity(13);
            write_frame_header(&mut resp, MSG_BULK_OK, 8);
            resp.extend_from_slice(&(count as u64).to_le_bytes());
            resp
        }
        Err(e) => make_error(format!("prevalidated bulk insert: {e}").as_bytes()),
    }
}

// ── Streaming bulk insert handlers ────────────────────────────────

/// Per-connection session state for the streaming bulk protocol.
/// Populated by MSG_BULK_STREAM_START, grown by each
/// MSG_BULK_STREAM_ROWS, drained + cleared by MSG_BULK_STREAM_COMMIT.
struct BulkStreamSession {
    collection: String,
    schema: std::sync::Arc<Vec<String>>,
    rows: Vec<Vec<crate::storage::schema::Value>>,
}

/// `MSG_BULK_STREAM_START` payload: `[coll_len u16][coll bytes]
/// [ncols u16]([name_len u16][name_bytes])*ncols`. Establishes the
/// collection + schema for subsequent ROWS frames. Any in-flight
/// session is aborted. Responds `MSG_BULK_STREAM_ACK` (empty body)
/// on success, `MSG_ERROR` otherwise.
fn handle_stream_start(payload: &[u8], session: &mut Option<BulkStreamSession>) -> Vec<u8> {
    let mut pos = 0;
    let coll_len = match read_u16(payload, &mut pos, "stream start: missing collection length") {
        Ok(len) => len as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let collection = match read_string(
        payload,
        &mut pos,
        coll_len,
        "stream start: truncated collection name",
        "stream start: invalid collection name",
    ) {
        Ok(s) => s,
        Err(msg) => return make_error(msg.as_bytes()),
    };

    let ncols = match read_u16(payload, &mut pos, "stream start: missing column count") {
        Ok(c) => c as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let mut names = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = match read_u16(
            payload,
            &mut pos,
            "stream start: missing column name length",
        ) {
            Ok(l) => l as usize,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        let name = match read_string(
            payload,
            &mut pos,
            name_len,
            "stream start: truncated column name",
            "stream start: invalid column name",
        ) {
            Ok(s) => s,
            Err(msg) => return make_error(msg.as_bytes()),
        };
        names.push(name);
    }

    *session = Some(BulkStreamSession {
        collection,
        schema: std::sync::Arc::new(names),
        rows: Vec::new(),
    });
    let mut resp = Vec::with_capacity(5);
    write_frame_header(&mut resp, MSG_BULK_STREAM_ACK, 0);
    resp
}

/// `MSG_BULK_STREAM_ROWS` payload: `[nrows u32] ([val_tag u8 +
/// val_data]*ncols)*nrows`. Columns are implicit from the session
/// schema so the frame carries only values. Responds
/// `MSG_BULK_STREAM_ACK` on success.
fn handle_stream_rows(payload: &[u8], session: &mut Option<BulkStreamSession>) -> Vec<u8> {
    let Some(state) = session.as_mut() else {
        return make_error(
            b"stream rows: no active stream session (send MSG_BULK_STREAM_START first)",
        );
    };
    let mut pos = 0;
    let nrows = match read_u32(payload, &mut pos, "stream rows: missing row count") {
        Ok(n) => n as usize,
        Err(msg) => return make_error(msg.as_bytes()),
    };
    let ncols = state.schema.len();
    state.rows.reserve(nrows);
    for _ in 0..nrows {
        let mut values = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let value = match try_decode_value(payload, &mut pos) {
                Ok(v) => v,
                Err(err) => return make_error(format!("stream rows: {err}").as_bytes()),
            };
            values.push(value);
        }
        state.rows.push(values);
    }
    // Success path emits NO response — the client pipelines the next
    // ROWS / COMMIT frame without waiting. Errors above return
    // `make_error(...)` so the client still sees a terminal response
    // on failure.
    Vec::new()
}

/// `MSG_BULK_STREAM_COMMIT` payload: empty. Finalises the streaming
/// session: flushes the accumulated rows via
/// `create_rows_batch_prevalidated_columnar` (the same fast path the
/// one-shot MSG_BULK_INSERT_PREVALIDATED uses) and responds with
/// `MSG_BULK_OK { count u64 }`. Session state is cleared whether
/// the flush succeeds or fails.
fn handle_stream_commit(
    runtime: &RedDBRuntime,
    session: &mut Option<BulkStreamSession>,
) -> Vec<u8> {
    let Some(state) = session.take() else {
        return make_error(b"stream commit: no active stream session");
    };
    if state.rows.is_empty() {
        let mut resp = Vec::with_capacity(13);
        write_frame_header(&mut resp, MSG_BULK_OK, 8);
        resp.extend_from_slice(&0u64.to_le_bytes());
        return resp;
    }
    match runtime.create_rows_batch_prevalidated_columnar(
        state.collection,
        state.schema,
        state.rows,
    ) {
        Ok(count) => {
            let mut resp = Vec::with_capacity(13);
            write_frame_header(&mut resp, MSG_BULK_OK, 8);
            resp.extend_from_slice(&(count as u64).to_le_bytes());
            resp
        }
        Err(e) => make_error(format!("stream commit: {e}").as_bytes()),
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
