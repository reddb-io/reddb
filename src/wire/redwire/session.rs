//! Per-connection v2 session: handshake → frame loop → bye.
//!
//! The dispatch loop is intentionally narrow in this initial cut.
//! It accepts:
//!   - Hello / AuthResponse (handshake only — once)
//!   - Query  → runs SQL, replies with one Result frame (JSON payload)
//!   - Ping   → Pong
//!   - Bye    → break loop
//!
//! Bulk inserts, prepared statements, multiplexed streams,
//! VectorSearch, GraphTraverse, Cancel — all land in subsequent
//! PRs. The framing + auth pieces are the load-bearing parts of
//! this PR; data-plane completeness is a follow-up.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};

use super::auth::{
    build_auth_fail, build_auth_ok, build_hello_ack, pick_auth_method, validate_auth_response,
    AuthOutcome, Hello,
};
use super::codec::{decode_frame, encode_frame};
use super::frame::{Frame, MessageKind, FRAME_HEADER_SIZE};
use super::{MAX_KNOWN_MINOR_VERSION, REDWIRE_V2_MAGIC};

#[derive(Debug)]
struct AuthedSession {
    username: String,
    #[allow(dead_code)]
    session_id: String,
}

pub async fn handle_session(
    mut stream: TcpStream,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
) -> io::Result<()> {
    // Discriminator byte was already consumed by the service-router
    // detector when it dispatched here. If callers wire this from
    // a non-router path they must consume it themselves first.
    let session = perform_handshake(&mut stream, auth_store.as_deref()).await?;
    if session.is_none() {
        return Ok(());
    }
    let _session = session.unwrap();

    let mut buf = vec![0u8; FRAME_HEADER_SIZE];
    loop {
        // Read header.
        if let Err(err) = stream.read_exact(&mut buf[..FRAME_HEADER_SIZE]).await {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(err);
        }
        let length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if length < FRAME_HEADER_SIZE || length > super::frame::MAX_FRAME_SIZE as usize {
            return Err(io::Error::other(format!("invalid frame length {length}")));
        }
        if buf.len() < length {
            buf.resize(length, 0);
        }
        let payload_len = length - FRAME_HEADER_SIZE;
        if payload_len > 0 {
            stream
                .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
                .await?;
        }
        let (frame, _) = decode_frame(&buf[..length])
            .map_err(|e| io::Error::other(format!("decode frame: {e}")))?;

        match frame.kind {
            MessageKind::Bye => {
                let bye = encode_frame(&Frame::new(MessageKind::Bye, frame.correlation_id, vec![]));
                let _ = stream.write_all(&bye).await;
                return Ok(());
            }
            MessageKind::Ping => {
                let pong = encode_frame(&Frame::new(MessageKind::Pong, frame.correlation_id, vec![]));
                stream.write_all(&pong).await?;
            }
            MessageKind::Query => {
                let response = run_query(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            // v1 inserted single rows via the BulkInsert code (with
            // a one-element array). v2 keeps that code; the payload
            // shape distinguishes single (`payload`) vs bulk (`payloads`).
            MessageKind::BulkInsert => {
                let response = run_insert_dispatch(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            MessageKind::Get => {
                let response = run_get(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            MessageKind::Delete => {
                let response = run_delete(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            other => {
                let err = encode_frame(&Frame::new(
                    MessageKind::Error,
                    frame.correlation_id,
                    format!("v2 server cannot dispatch {other:?} yet").into_bytes(),
                ));
                stream.write_all(&err).await?;
            }
        }
    }
}

/// Run the handshake. Returns `Ok(None)` when the client disconnected
/// or the auth was refused (the failure frame is already on the wire).
async fn perform_handshake(
    stream: &mut TcpStream,
    auth_store: Option<&AuthStore>,
) -> io::Result<Option<AuthedSession>> {
    // Step 1: read minor version byte.
    let mut minor_buf = [0u8; 1];
    stream.read_exact(&mut minor_buf).await?;
    let minor = minor_buf[0];
    if minor > MAX_KNOWN_MINOR_VERSION {
        // Future client speaking a version we don't know — refuse
        // immediately. We do not send a frame because the client
        // hasn't agreed on the framing version yet.
        return Ok(None);
    }

    // Step 2: read the Hello frame.
    let hello = read_frame(stream).await?;
    if hello.kind != MessageKind::Hello {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            hello.correlation_id,
            build_auth_fail("first frame after magic must be Hello"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let hello_msg = match Hello::from_payload(&hello.payload) {
        Ok(h) => h,
        Err(e) => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                hello.correlation_id,
                build_auth_fail(&e),
            ));
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    let chosen_version = hello_msg
        .versions
        .iter()
        .copied()
        .filter(|v| *v <= MAX_KNOWN_MINOR_VERSION)
        .max()
        .unwrap_or(0);
    if chosen_version == 0 {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            hello.correlation_id,
            build_auth_fail("no overlapping protocol version"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    let server_anon_ok = auth_store.map(|s| !s.is_enabled()).unwrap_or(true);
    let chosen = match pick_auth_method(&hello_msg.auth_methods, server_anon_ok) {
        Some(m) => m,
        None => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                hello.correlation_id,
                build_auth_fail("no overlapping auth method"),
            ));
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // Step 3: HelloAck.
    let server_features = 0u32;
    let ack = encode_frame(&Frame::new(
        MessageKind::HelloAck,
        hello.correlation_id,
        build_hello_ack(chosen_version, chosen, server_features),
    ));
    stream.write_all(&ack).await?;

    // Step 4: AuthResponse (no challenge in v2.1 since bearer/anonymous
    // are zero-round-trip).
    let resp = read_frame(stream).await?;
    if resp.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            resp.correlation_id,
            build_auth_fail("expected AuthResponse"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    match validate_auth_response(chosen, &resp.payload, auth_store) {
        AuthOutcome::Authenticated {
            username,
            role,
            session_id,
        } => {
            let ok = encode_frame(&Frame::new(
                MessageKind::AuthOk,
                resp.correlation_id,
                build_auth_ok(&session_id, &username, role, server_features),
            ));
            stream.write_all(&ok).await?;
            Ok(Some(AuthedSession {
                username,
                session_id,
            }))
        }
        AuthOutcome::Refused(reason) => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                resp.correlation_id,
                build_auth_fail(&reason),
            ));
            let _ = stream.write_all(&fail).await;
            Ok(None)
        }
    }
}

async fn read_frame(stream: &mut TcpStream) -> io::Result<Frame> {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    stream.read_exact(&mut header).await?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if length < FRAME_HEADER_SIZE || length > super::frame::MAX_FRAME_SIZE as usize {
        return Err(io::Error::other(format!(
            "redwire frame length {length} out of range"
        )));
    }
    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        stream
            .read_exact(&mut buf[FRAME_HEADER_SIZE..length])
            .await?;
    }
    let (frame, _) = decode_frame(&buf)
        .map_err(|e| io::Error::other(format!("decode frame: {e}")))?;
    Ok(frame)
}

fn run_query(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let sql = match std::str::from_utf8(&frame.payload) {
        Ok(s) => s,
        Err(_) => {
            return error_frame(frame.correlation_id, "Query payload must be UTF-8 SQL");
        }
    };
    match runtime.execute_query(sql) {
        Ok(result) => {
            let mut obj = crate::serde_json::Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(true));
            obj.insert(
                "statement".to_string(),
                JsonValue::String(result.statement_type.to_string()),
            );
            obj.insert(
                "affected".to_string(),
                JsonValue::Number(result.affected_rows as f64),
            );
            let payload = serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default();
            Frame::new(MessageKind::Result, frame.correlation_id, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

/// Insert dispatch — handles both single-row and bulk shapes off
/// the same `BulkInsert` (0x04) frame:
///   - `{ "collection": "...", "payload": {...} }` → single insert
///   - `{ "collection": "...", "payloads": [...] }` → bulk insert
///
/// Mirrors the JSON-RPC `insert` / `bulk_insert` method shapes
/// from `rpc_stdio.rs` so both transports agree on the payload.
fn run_insert_dispatch(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let v: JsonValue = match serde_json::from_slice(&frame.payload) {
        Ok(v) => v,
        Err(e) => return error_frame(frame.correlation_id, &format!("Insert: invalid JSON: {e}")),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return error_frame(frame.correlation_id, "Insert: payload must be a JSON object"),
    };
    let collection = match obj.get("collection").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Insert: missing 'collection' string"),
    };

    if let Some(rows) = obj.get("payloads").and_then(|x| x.as_array()) {
        let mut affected: u64 = 0;
        for entry in rows {
            let row = match entry.as_object() {
                Some(o) => o,
                None => return error_frame(
                    frame.correlation_id,
                    "Insert: each payload must be a JSON object",
                ),
            };
            let sql = crate::rpc_stdio::build_insert_sql(collection, row.iter());
            match runtime.execute_query(&sql) {
                Ok(qr) => affected += qr.affected_rows,
                Err(err) => return error_frame(frame.correlation_id, &err.to_string()),
            }
        }
        let mut out = crate::serde_json::Map::new();
        out.insert("affected".to_string(), JsonValue::Number(affected as f64));
        let payload = serde_json::to_vec(&JsonValue::Object(out)).unwrap_or_default();
        return Frame::new(MessageKind::BulkOk, frame.correlation_id, payload);
    }

    let row = match obj.get("payload").and_then(|x| x.as_object()) {
        Some(o) => o,
        None => return error_frame(
            frame.correlation_id,
            "Insert: missing 'payload' object or 'payloads' array",
        ),
    };
    let sql = crate::rpc_stdio::build_insert_sql(collection, row.iter());
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let body = crate::rpc_stdio::insert_result_to_json(&qr);
            let payload = serde_json::to_vec(&body).unwrap_or_default();
            Frame::new(MessageKind::BulkOk, frame.correlation_id, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

fn error_frame(correlation_id: u64, msg: &str) -> Frame {
    Frame::new(MessageKind::Error, correlation_id, msg.as_bytes().to_vec())
}

/// Get payload shape: `{ "collection": "...", "id": "..." }`.
/// Bridges to `SELECT * FROM <coll> WHERE _id = '<id>' LIMIT 1`.
/// Reply: Result frame with the row, or empty `{}` when not found.
fn run_get(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let v: JsonValue = match serde_json::from_slice(&frame.payload) {
        Ok(v) => v,
        Err(e) => return error_frame(frame.correlation_id, &format!("Get: invalid JSON: {e}")),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return error_frame(frame.correlation_id, "Get: payload must be a JSON object"),
    };
    let collection = match obj.get("collection").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Get: missing 'collection' string"),
    };
    let id = match obj.get("id").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Get: missing 'id' string"),
    };
    // Sanitise the id by treating it as a string literal — same
    // approach as build_insert_sql for arbitrary input.
    let id_lit = crate::rpc_stdio::value_to_sql_literal(&JsonValue::String(id.to_string()));
    let sql = format!("SELECT * FROM {collection} WHERE _id = {id_lit} LIMIT 1");
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let mut out = crate::serde_json::Map::new();
            out.insert("ok".to_string(), JsonValue::Bool(true));
            out.insert(
                "found".to_string(),
                JsonValue::Bool(!qr.result.records.is_empty()),
            );
            // Records pass through as-is; the JS / Rust clients
            // pick the shape they want from the JSON envelope.
            let payload = serde_json::to_vec(&JsonValue::Object(out)).unwrap_or_default();
            Frame::new(MessageKind::Result, frame.correlation_id, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

/// Delete payload shape: `{ "collection": "...", "id": "..." }`.
/// Bridges to `DELETE FROM <coll> WHERE _id = '<id>'`.
/// Reply: DeleteOk frame with `{ affected }`.
fn run_delete(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let v: JsonValue = match serde_json::from_slice(&frame.payload) {
        Ok(v) => v,
        Err(e) => return error_frame(frame.correlation_id, &format!("Delete: invalid JSON: {e}")),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return error_frame(frame.correlation_id, "Delete: payload must be a JSON object"),
    };
    let collection = match obj.get("collection").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Delete: missing 'collection' string"),
    };
    let id = match obj.get("id").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Delete: missing 'id' string"),
    };
    let id_lit = crate::rpc_stdio::value_to_sql_literal(&JsonValue::String(id.to_string()));
    let sql = format!("DELETE FROM {collection} WHERE _id = {id_lit}");
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let mut out = crate::serde_json::Map::new();
            out.insert(
                "affected".to_string(),
                JsonValue::Number(qr.affected_rows as f64),
            );
            let payload = serde_json::to_vec(&JsonValue::Object(out)).unwrap_or_default();
            Frame::new(MessageKind::DeleteOk, frame.correlation_id, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_byte_is_0xfe() {
        assert_eq!(REDWIRE_V2_MAGIC, 0xFE);
    }
}
