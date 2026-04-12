/// RedDB Wire Protocol TCP Listener (plaintext + TLS)
///
/// Accepts TCP connections and processes binary wire protocol messages.
/// Each connection is handled in its own tokio task.
/// Supports both plaintext TCP and TLS-encrypted connections.
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use super::protocol::*;
use crate::runtime::RedDBRuntime;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, EntityId, EntityKind};

/// Start the wire protocol TCP listener (plaintext).
pub async fn start_wire_listener(
    bind_addr: &str,
    runtime: Arc<RedDBRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    eprintln!("red server (wire) listening on {bind_addr}");

    loop {
        let (stream, _addr) = listener.accept().await?;
        let rt = runtime.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, rt).await {
                eprintln!("wire connection error: {e}");
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
    eprintln!("red server (wire+tls) listening on {bind_addr}");

    loop {
        let (tcp_stream, _addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let rt = runtime.clone();
        tokio::spawn(async move {
            match acceptor.accept(tcp_stream).await {
                Ok(tls_stream) => {
                    if let Err(e) = handle_connection(tls_stream, rt).await {
                        eprintln!("wire+tls connection error: {e}");
                    }
                }
                Err(e) => eprintln!("wire TLS handshake failed: {e}"),
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

        let total_len = u32::from_le_bytes(header_buf[0..4].try_into().unwrap()) as usize;
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
    if let Some(entity_id) =
        crate::runtime::query_exec::extract_entity_id_from_filter(&table_query.filter)
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

    let filter = table_query.filter.as_ref();
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
                "_entity_id".into(),
                "_created_at".into(),
                "_updated_at".into(),
            ];
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    cols.extend(named.keys().cloned());
                }
            }
            col_names = Some(cols);
        }

        let cols = col_names.as_ref().unwrap();
        let mut row = Vec::with_capacity(cols.len() * 10);
        for col in cols {
            let val = match col.as_str() {
                "_entity_id" => Value::UnsignedInteger(entity.id.raw()),
                "_created_at" => Value::UnsignedInteger(entity.created_at),
                "_updated_at" => Value::UnsignedInteger(entity.updated_at),
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
        "_entity_id".into(),
        "_created_at".into(),
        "_updated_at".into(),
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
            "_entity_id" => Value::UnsignedInteger(entity.id.raw()),
            "_created_at" => Value::UnsignedInteger(entity.created_at),
            "_updated_at" => Value::UnsignedInteger(entity.updated_at),
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
    let coll_len = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    let collection = match std::str::from_utf8(&payload[pos..pos + coll_len]) {
        Ok(s) => s.to_string(),
        Err(_) => return make_error(b"invalid collection name"),
    };
    pos += coll_len;

    // Number of rows
    let nrows = u32::from_le_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    // Parse JSON payloads
    let mut json_payloads = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let json_len = u32::from_le_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let json_str = match std::str::from_utf8(&payload[pos..pos + json_len]) {
            Ok(s) => s.to_string(),
            Err(_) => return make_error(b"invalid JSON payload"),
        };
        pos += json_len;
        json_payloads.push(json_str);
    }

    // Execute bulk insert via existing gRPC path.
    //
    // Optimization: build the columnar `RowData` representation with a
    // SHARED schema taken from the first row. All subsequent rows reuse
    // the same `Arc<Vec<String>>` and a shared `Arc<str>` for the table
    // name — no per-row HashMap, no per-cell string clones. Rows whose
    // keyset differs from the first are skipped to preserve the shape.
    let store = runtime.db().store();
    let mut entities = Vec::with_capacity(nrows);
    let table: Arc<str> = Arc::from(collection.as_str());
    let mut schema_arc: Option<Arc<Vec<String>>> = None;

    for json_str in &json_payloads {
        let parsed: crate::json::Value = match crate::json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => return make_error(format!("JSON parse: {e}").as_bytes()),
        };
        let fields = match parsed.get("fields").and_then(|f| f.as_object()) {
            Some(f) => f,
            None => return make_error(b"missing 'fields' object"),
        };

        // On the first row, freeze the column order as the schema.
        let schema = match &schema_arc {
            Some(s) => Arc::clone(s),
            None => {
                let names: Vec<String> = fields.keys().cloned().collect();
                let arc = Arc::new(names);
                schema_arc = Some(Arc::clone(&arc));
                arc
            }
        };

        let mut columns = Vec::with_capacity(schema.len());
        for name in schema.iter() {
            let val = match fields.get(name) {
                Some(v) => v,
                None => {
                    columns.push(Value::Null);
                    continue;
                }
            };
            let value = match val {
                crate::json::Value::String(s) => Value::Text(s.clone()),
                crate::json::Value::Number(n) => {
                    if n.fract().abs() < f64::EPSILON {
                        Value::Integer(*n as i64)
                    } else {
                        Value::Float(*n)
                    }
                }
                crate::json::Value::Bool(b) => Value::Boolean(*b),
                crate::json::Value::Null => Value::Null,
                _ => Value::Null,
            };
            columns.push(value);
        }

        entities.push(crate::storage::unified::UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::clone(&table),
                row_id: 0,
            },
            EntityData::Row(crate::storage::unified::RowData {
                columns,
                named: None,
                schema: Some(schema),
            }),
        ));
    }

    match store.bulk_insert(&collection, entities) {
        Ok(ids) => {
            let count = ids.len() as u64;
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
        first.values.keys().cloned().collect()
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
            let value = record.values.get(col).unwrap_or(&Value::Null);
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
    let coll_len = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    let collection = match std::str::from_utf8(&payload[pos..pos + coll_len]) {
        Ok(s) => s.to_string(),
        Err(_) => return make_error(b"invalid collection name"),
    };
    pos += coll_len;

    // Column names
    let ncols = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    let mut col_names = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let name = std::str::from_utf8(&payload[pos..pos + name_len])
            .unwrap_or("?")
            .to_string();
        pos += name_len;
        col_names.push(name);
    }

    // Number of rows
    let nrows = u32::from_le_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    // Decode rows into entities using the columnar `RowData` path:
    // one shared `Arc<Vec<String>>` for column names, one shared
    // `Arc<str>` for the table, and `Vec<Value>` per row — zero
    // HashMap allocations, zero per-cell string clones.
    let table: Arc<str> = Arc::from(collection.as_str());
    let schema: Arc<Vec<String>> = Arc::new(col_names);
    let mut entities = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let mut columns = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            columns.push(decode_value(payload, &mut pos));
        }
        entities.push(crate::storage::unified::UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::clone(&table),
                row_id: 0,
            },
            EntityData::Row(crate::storage::unified::RowData {
                columns,
                named: None,
                schema: Some(Arc::clone(&schema)),
            }),
        ));
    }

    // Bulk insert
    let store = runtime.db().store();
    match store.bulk_insert(&collection, entities) {
        Ok(ids) => {
            let count = ids.len() as u64;
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
