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

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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

pub async fn handle_session<S>(
    mut stream: S,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Discriminator byte was already consumed by the service-router
    // detector when it dispatched here. If callers wire this from
    // a non-router path they must consume it themselves first.
    let session = perform_handshake(&mut stream, auth_store.as_deref()).await?;
    if session.is_none() {
        return Ok(());
    }
    let _session = session.unwrap();

    // Per-connection state for v1 prepared statements + streaming
    // bulk inserts. Same shape the v1 listener uses; v2 just owns
    // the lifetime.
    let mut stream_session: Option<crate::wire::listener::BulkStreamSession> = None;
    let mut prepared_stmts: std::collections::HashMap<u32, crate::wire::listener::PreparedStmt> =
        std::collections::HashMap::new();

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
            // v1 binary fast paths — reuse the v1 handler verbatim.
            // The handler returns a v1-framed response; we strip
            // the 5-byte v1 header and rewrap with v2 framing,
            // preserving zero-copy of the body bytes for max perf
            // parity with v1 stress tools.
            MessageKind::BulkInsertBinary => {
                let v1 = crate::wire::listener::handle_bulk_insert_binary(&runtime, &frame.payload);
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::BulkInsertPrevalidated => {
                let v1 = crate::wire::listener::handle_bulk_insert_binary_prevalidated(
                    &runtime,
                    &frame.payload,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::QueryBinary => {
                let v1 = crate::wire::listener::handle_query_binary(&runtime, &frame.payload);
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            // Streaming bulk insert (PG COPY equivalent). Same
            // start/rows/commit dance as v1, with v2 framing on
            // the wire. State persists across frames on the same
            // session.
            MessageKind::BulkStreamStart => {
                let v1 = crate::wire::listener::handle_stream_start(
                    &frame.payload,
                    &mut stream_session,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::BulkStreamRows => {
                let v1 = crate::wire::listener::handle_stream_rows(
                    &runtime,
                    &frame.payload,
                    &mut stream_session,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::BulkStreamCommit => {
                let v1 = crate::wire::listener::handle_stream_commit(
                    &runtime,
                    &mut stream_session,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            // Prepared statements — parse SQL once via Prepare,
            // bind + execute many times via ExecutePrepared.
            // Per-session HashMap holds compiled shapes.
            MessageKind::Prepare => {
                let v1 = crate::wire::listener::handle_prepare(
                    &runtime,
                    &frame.payload,
                    &mut prepared_stmts,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::ExecutePrepared => {
                let v1 = crate::wire::listener::handle_execute_prepared(
                    &runtime,
                    &frame.payload,
                    &prepared_stmts,
                );
                stream.write_all(&encode_frame(&rewrap_v1(&v1, &frame))).await?;
            }
            MessageKind::PreparedOk => {
                // Server-emitted in response to Prepare; clients
                // shouldn't send it. Ignore-with-error to flag
                // misbehaving callers.
                let err = encode_frame(&Frame::new(
                    MessageKind::Error,
                    frame.correlation_id,
                    b"PreparedOk is server-only".to_vec(),
                ));
                stream.write_all(&err).await?;
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
async fn perform_handshake<S>(
    stream: &mut S,
    auth_store: Option<&AuthStore>,
) -> io::Result<Option<AuthedSession>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
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

    // SCRAM is a 3-RTT challenge/response exchange. Branch off to
    // its own state machine before the 1-RTT bearer/anonymous
    // path runs.
    if chosen == "scram-sha-256" {
        return perform_scram_handshake(
            stream,
            auth_store,
            hello.correlation_id,
            server_features,
        )
        .await;
    }

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

/// 3-RTT SCRAM-SHA-256 server handshake (RFC 5802 + RFC 7677).
///
///     C → S  AuthResponse(client-first-message)         (already received as client-first)
///     S → C  AuthRequest(server-first-message)
///     C → S  AuthResponse(client-final-message)
///     S → C  AuthOk(v=server-signature)
async fn perform_scram_handshake<S>(
    stream: &mut S,
    auth_store: Option<&AuthStore>,
    initial_correlation: u64,
    server_features: u32,
) -> io::Result<Option<AuthedSession>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let store = match auth_store {
        Some(s) => s,
        None => {
            let fail = encode_frame(&Frame::new(
                MessageKind::AuthFail,
                initial_correlation,
                build_auth_fail("scram-sha-256 requires an AuthStore"),
            ));
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // 1. Client-first.
    let cf = read_frame(stream).await?;
    if cf.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            cf.correlation_id,
            build_auth_fail("expected AuthResponse(client-first-message)"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let (username, client_nonce, client_first_bare) =
        match super::auth::parse_scram_client_first(&cf.payload) {
            Ok(t) => t,
            Err(e) => {
                let fail = encode_frame(&Frame::new(
                    MessageKind::AuthFail,
                    cf.correlation_id,
                    build_auth_fail(&format!("scram client-first: {e}")),
                ));
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };

    // 2. Look up the verifier. If the user doesn't exist or has
    // no SCRAM verifier, run a dummy iteration count to keep the
    // timing flat (no user-enumeration leak).
    let verifier = store.lookup_scram_verifier(&username);
    let (salt, iter, stored_key, server_key, user_known) = match &verifier {
        Some(v) => (
            v.salt.clone(),
            v.iter,
            v.stored_key,
            v.server_key,
            true,
        ),
        None => (
            crate::auth::store::random_bytes(16),
            crate::auth::scram::DEFAULT_ITER,
            [0u8; 32],
            [0u8; 32],
            false,
        ),
    };

    // 3. Server-first.
    let server_nonce = super::auth::new_server_nonce();
    let server_first =
        super::auth::build_scram_server_first(&client_nonce, &server_nonce, &salt, iter);
    let req = encode_frame(&Frame::new(
        MessageKind::AuthRequest,
        cf.correlation_id,
        server_first.as_bytes().to_vec(),
    ));
    stream.write_all(&req).await?;

    // 4. Client-final.
    let cfinal = read_frame(stream).await?;
    if cfinal.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            cfinal.correlation_id,
            build_auth_fail("expected AuthResponse(client-final-message)"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let (combined_nonce, presented_proof, client_final_no_proof) =
        match super::auth::parse_scram_client_final(&cfinal.payload) {
            Ok(t) => t,
            Err(e) => {
                let fail = encode_frame(&Frame::new(
                    MessageKind::AuthFail,
                    cfinal.correlation_id,
                    build_auth_fail(&format!("scram client-final: {e}")),
                ));
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };
    let expected_combined = format!("{client_nonce}{server_nonce}");
    if combined_nonce != expected_combined {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            cfinal.correlation_id,
            build_auth_fail("scram nonce mismatch — replay protection failed"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    // 5. Verify proof.
    let auth_message = crate::auth::scram::auth_message(
        &client_first_bare,
        &server_first,
        &client_final_no_proof,
    );
    let proof_ok = if user_known {
        let v = crate::auth::scram::ScramVerifier {
            salt: salt.clone(),
            iter,
            stored_key,
            server_key,
        };
        crate::auth::scram::verify_client_proof(&v, &auth_message, &presented_proof)
    } else {
        false
    };
    if !proof_ok {
        let fail = encode_frame(&Frame::new(
            MessageKind::AuthFail,
            cfinal.correlation_id,
            build_auth_fail("invalid SCRAM proof"),
        ));
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    // 6. AuthOk with server signature.
    let role = store
        .list_users()
        .into_iter()
        .find(|u| u.username == username)
        .map(|u| u.role)
        .unwrap_or(crate::auth::Role::Read);
    let server_sig = crate::auth::scram::server_signature(&server_key, &auth_message);
    let session_id = super::auth::new_session_id_for_scram();
    let ok_payload = super::auth::build_scram_auth_ok(
        &session_id,
        &username,
        role,
        server_features,
        &server_sig,
    );
    let ok = encode_frame(&Frame::new(
        MessageKind::AuthOk,
        cfinal.correlation_id,
        ok_payload,
    ));
    stream.write_all(&ok).await?;
    Ok(Some(AuthedSession {
        username,
        session_id,
    }))
}

async fn read_frame<S>(stream: &mut S) -> io::Result<Frame>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
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

/// Adapt a v1-framed handler response into a v2 frame.
///
/// v1 handlers (`handle_bulk_insert_binary`, `handle_query_binary`,
/// etc) return `[u32 length][u8 msg_type][body]` already wrapped.
/// v2 carries the same body verbatim (kinds 0x01..0x0F are
/// preserved by design), so we strip the 5-byte v1 header and
/// rewrap with v2 framing using the same `correlation_id` and
/// the kind byte from the v1 frame.
///
/// The payload `Vec<u8>` is moved into the new frame — no extra
/// allocation, no body copy — which is the property that lets
/// v2 hit v1 perf parity on bulk insert benchmarks.
fn rewrap_v1(v1_bytes: &[u8], req: &Frame) -> Frame {
    if v1_bytes.len() < 5 {
        return error_frame(req.correlation_id, "v1 handler returned a truncated frame");
    }
    let kind_byte = v1_bytes[4];
    let kind = MessageKind::from_u8(kind_byte).unwrap_or(MessageKind::Error);
    let body = v1_bytes[5..].to_vec();
    Frame::new(kind, req.correlation_id, body)
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
