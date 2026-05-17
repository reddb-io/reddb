//! Per-connection RedWire session: handshake → frame loop → bye.
//!
//! Dispatches the full RedWire frame set:
//!   - Hello / AuthResponse (handshake only — once)
//!   - Query / BulkInsert / Get / Delete (data plane)
//!   - QueryBinary / BulkInsertBinary / BulkInsertPrevalidated
//!     (binary fast paths)
//!   - BulkStreamStart/Rows/Commit (streaming bulk)
//!   - Prepare / ExecutePrepared (prepared statements)
//!   - Ping / Pong / Bye (lifecycle)

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::store::AuthStore;
use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};
use reddb_wire::query_with_params::{
    decode_query_with_params, ParamValue as RedWireParamValue, FEATURE_PARAMS,
};

use super::auth::{
    build_auth_fail, build_auth_ok, build_hello_ack, pick_auth_method, validate_auth_response,
    AuthOutcome, Hello,
};
use super::codec::{decode_frame, encode_frame};
use super::frame::{Frame, MessageDirection, MessageKind, FRAME_HEADER_SIZE};
use super::{FrameBuilder, MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC};

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
    oauth: Option<Arc<crate::auth::oauth::OAuthValidator>>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Discriminator byte was already consumed by the service-router
    // detector when it dispatched here. If callers wire this from
    // a non-router path they must consume it themselves first.
    let session = perform_handshake(
        &mut stream,
        runtime.as_ref(),
        auth_store.as_deref(),
        oauth.as_deref(),
    )
    .await?;
    if session.is_none() {
        return Ok(());
    }
    let _session = session.unwrap();

    // Per-connection state for prepared statements + streaming
    // bulk inserts. Owned by the session; dropped on disconnect.
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

        // Catalog-driven direction gate: server-only kinds (PreparedOk,
        // AuthOk/Fail, BulkOk, …) must never arrive *from* a client.
        // The catalog (`MessageKind::direction`) is the single source
        // of truth — see `frame.rs::catalog_tests::direction_matrix_is_pinned`.
        if frame.kind.direction() == MessageDirection::ServerToClient {
            let err_frame = FrameBuilder::reply_to(frame.correlation_id)
                .kind(MessageKind::Error)
                .payload(format!("redwire: {:?} is server-only", frame.kind).into_bytes())
                .build()
                .map_err(|e| io::Error::other(format!("build Error frame: {e}")))?;
            stream.write_all(&encode_frame(&err_frame)).await?;
            continue;
        }

        match frame.kind {
            MessageKind::Bye => {
                let bye = encode_frame(&build_reply(
                    frame.correlation_id,
                    MessageKind::Bye,
                    vec![],
                )?);
                let _ = stream.write_all(&bye).await;
                return Ok(());
            }
            MessageKind::Ping => {
                let pong = encode_frame(&build_reply(
                    frame.correlation_id,
                    MessageKind::Pong,
                    vec![],
                )?);
                stream.write_all(&pong).await?;
            }
            MessageKind::Query => {
                let response = run_query(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            MessageKind::QueryWithParams => {
                let response = run_query_with_params(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            // BulkInsert handles both single-row and bulk shapes off
            // the same frame kind: payload `payload` = single,
            // payload `payloads` = array.
            MessageKind::BulkInsert => {
                let response = run_insert_dispatch(&runtime, &frame);
                stream.write_all(&encode_frame(&response)).await?;
            }
            // Binary fast paths — handlers produce a length-prefixed
            // body which we extract and rewrap as a RedWire frame,
            // moving the body Vec without a copy.
            MessageKind::BulkInsertBinary => {
                let raw =
                    crate::wire::listener::handle_bulk_insert_binary(&runtime, &frame.payload);
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            MessageKind::BulkInsertPrevalidated => {
                let raw = crate::wire::listener::handle_bulk_insert_binary_prevalidated(
                    &runtime,
                    &frame.payload,
                );
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            MessageKind::QueryBinary => {
                let raw = crate::wire::listener::handle_query_binary(&runtime, &frame.payload);
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            // Streaming bulk insert (PG COPY equivalent).
            // start/rows/commit; per-session state persists across frames.
            MessageKind::BulkStreamStart => {
                let raw =
                    crate::wire::listener::handle_stream_start(&frame.payload, &mut stream_session);
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            MessageKind::BulkStreamRows => {
                let raw = crate::wire::listener::handle_stream_rows(
                    &runtime,
                    &frame.payload,
                    &mut stream_session,
                );
                // The legacy handler signals the success no-response
                // path with an empty Vec — the client pipelines the
                // next ROWS / COMMIT frame without an ack. Errors come
                // back as a non-empty length-prefixed frame and must
                // be forwarded so the client sees a terminal response.
                if !raw.is_empty() {
                    stream
                        .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                        .await?;
                }
            }
            MessageKind::BulkStreamCommit => {
                let raw =
                    crate::wire::listener::handle_stream_commit(&runtime, &mut stream_session);
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            // Prepared statements — parse SQL once via Prepare,
            // bind + execute many times via ExecutePrepared.
            // Per-session HashMap holds compiled shapes.
            MessageKind::Prepare => {
                let raw = crate::wire::listener::handle_prepare(
                    &runtime,
                    &frame.payload,
                    &mut prepared_stmts,
                );
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
            }
            MessageKind::ExecutePrepared => {
                let raw = crate::wire::listener::handle_execute_prepared(
                    &runtime,
                    &frame.payload,
                    &prepared_stmts,
                );
                stream
                    .write_all(&encode_frame(&rewrap_handler_response(&raw, &frame)))
                    .await?;
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
                let err_frame = FrameBuilder::reply_to(frame.correlation_id)
                    .kind(MessageKind::Error)
                    .payload(format!("redwire: cannot dispatch {other:?} yet").into_bytes())
                    .build()
                    .map_err(|e| io::Error::other(format!("build Error frame: {e}")))?;
                let err = encode_frame(&err_frame);
                stream.write_all(&err).await?;
            }
        }
    }
}

/// Run the handshake. Returns `Ok(None)` when the client disconnected
/// or the auth was refused (the failure frame is already on the wire).
async fn perform_handshake<S>(
    stream: &mut S,
    runtime: &RedDBRuntime,
    auth_store: Option<&AuthStore>,
    oauth: Option<&crate::auth::oauth::OAuthValidator>,
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
        let fail = encode_frame(&build_reply(
            hello.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("first frame after magic must be Hello"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let hello_msg = match Hello::from_payload(&hello.payload) {
        Ok(h) => h,
        Err(e) => {
            let fail = encode_frame(&build_reply(
                hello.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail(&e),
            )?);
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
        let fail = encode_frame(&build_reply(
            hello.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("no overlapping protocol version"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    let server_anon_ok = auth_store.map(|s| !s.is_enabled()).unwrap_or(true);
    let chosen = match pick_auth_method(&hello_msg.auth_methods, server_anon_ok) {
        Some(m) => m,
        None => {
            let fail = encode_frame(&build_reply(
                hello.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail("no overlapping auth method"),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // Step 3: HelloAck.
    //
    // HelloAck is sent before any AuthResponse arrives, so the
    // caller is unauthenticated at this point. The TopologyAdvertiser
    // collapses anonymous to primary-only per ADR 0008 §3 — that's
    // the correct payload for the bootstrap path. Authenticated
    // principals get the full replica list via the gRPC `Topology`
    // RPC after the connection is established.
    let server_features = FEATURE_PARAMS;
    let topology = build_topology_for_hello_ack(runtime);
    let ack_frame = FrameBuilder::reply_to(hello.correlation_id)
        .kind(MessageKind::HelloAck)
        .payload(build_hello_ack(
            chosen_version,
            chosen,
            server_features,
            topology.as_ref(),
        ))
        .build()
        .map_err(|e| io::Error::other(format!("build HelloAck: {e}")))?;
    let ack = encode_frame(&ack_frame);
    stream.write_all(&ack).await?;

    // SCRAM is a 3-RTT challenge/response exchange. Branch off to
    // its own state machine before the 1-RTT bearer/anonymous
    // path runs.
    if chosen == "scram-sha-256" {
        return perform_scram_handshake(stream, auth_store, hello.correlation_id, server_features)
            .await;
    }

    // Step 4: AuthResponse (no challenge for the 1-RTT methods —
    // bearer/anonymous send their proof in the first AuthResponse).
    let resp = read_frame(stream).await?;
    if resp.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&build_reply(
            resp.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("expected AuthResponse"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    // OAuth-JWT branch — needs the validator that the 1-RTT
    // shape doesn't get. Done inline so the rest of the path
    // (anonymous / bearer) keeps the same bookkeeping.
    if chosen == "oauth-jwt" {
        let validator = match oauth {
            Some(v) => v,
            None => {
                let fail = encode_frame(&build_reply(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail("oauth-jwt requires RedWireConfig.oauth"),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };
        let raw = match crate::serde_json::from_slice::<JsonValue>(&resp.payload)
            .ok()
            .and_then(|v| {
                v.as_object()
                    .and_then(|o| o.get("jwt").cloned())
                    .and_then(|x| x.as_str().map(String::from))
            }) {
            Some(s) if !s.is_empty() => s,
            _ => {
                let fail = encode_frame(&build_reply(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail("oauth-jwt: AuthResponse missing 'jwt' string"),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };
        match super::auth::validate_oauth_jwt(validator, &raw) {
            Ok((username, role)) => {
                let session_id = super::auth::new_session_id_for_scram();
                let ok = encode_frame(&build_reply(
                    resp.correlation_id,
                    MessageKind::AuthOk,
                    build_auth_ok(&session_id, &username, role, server_features),
                )?);
                stream.write_all(&ok).await?;
                return Ok(Some(AuthedSession {
                    username,
                    session_id,
                }));
            }
            Err(reason) => {
                let fail = encode_frame(&build_reply(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail(&format!("oauth-jwt: {reason}")),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        }
    }

    match validate_auth_response(chosen, &resp.payload, auth_store) {
        AuthOutcome::Authenticated {
            username,
            role,
            session_id,
        } => {
            let ok_frame = FrameBuilder::reply_to(resp.correlation_id)
                .kind(MessageKind::AuthOk)
                .payload(build_auth_ok(&session_id, &username, role, server_features))
                .build()
                .map_err(|e| io::Error::other(format!("build AuthOk: {e}")))?;
            let ok = encode_frame(&ok_frame);
            stream.write_all(&ok).await?;
            Ok(Some(AuthedSession {
                username,
                session_id,
            }))
        }
        AuthOutcome::Refused(reason) => {
            let fail = encode_frame(&build_reply(
                resp.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail(&reason),
            )?);
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
            let fail = encode_frame(&build_reply(
                initial_correlation,
                MessageKind::AuthFail,
                build_auth_fail("scram-sha-256 requires an AuthStore"),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // 1. Client-first.
    let cf = read_frame(stream).await?;
    if cf.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&build_reply(
            cf.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("expected AuthResponse(client-first-message)"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let (username, client_nonce, client_first_bare) =
        match super::auth::parse_scram_client_first(&cf.payload) {
            Ok(t) => t,
            Err(e) => {
                let fail = encode_frame(&build_reply(
                    cf.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail(&format!("scram client-first: {e}")),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };

    // 2. Look up the verifier. The wire handshake doesn't yet learn
    // a tenant before the SCRAM exchange completes, so we resolve
    // against the platform tenant. Tenant-scoped users authenticate
    // through the JWT path (which carries the tenant claim) or a
    // future explicit `tenant` extension to the AuthRequest payload.
    // If the user doesn't exist or has no SCRAM verifier, run a
    // dummy iteration count to keep the timing flat
    // (no user-enumeration leak).
    let verifier = store.lookup_scram_verifier_global(&username);
    let (salt, iter, stored_key, server_key, user_known) = match &verifier {
        Some(v) => (v.salt.clone(), v.iter, v.stored_key, v.server_key, true),
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
    let req = encode_frame(&build_reply(
        cf.correlation_id,
        MessageKind::AuthRequest,
        server_first.as_bytes().to_vec(),
    )?);
    stream.write_all(&req).await?;

    // 4. Client-final.
    let cfinal = read_frame(stream).await?;
    if cfinal.kind != MessageKind::AuthResponse {
        let fail = encode_frame(&build_reply(
            cfinal.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("expected AuthResponse(client-final-message)"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let (combined_nonce, presented_proof, client_final_no_proof) =
        match super::auth::parse_scram_client_final(&cfinal.payload) {
            Ok(t) => t,
            Err(e) => {
                let fail = encode_frame(&build_reply(
                    cfinal.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail(&format!("scram client-final: {e}")),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };
    let expected_combined = format!("{client_nonce}{server_nonce}");
    if combined_nonce != expected_combined {
        let fail = encode_frame(&build_reply(
            cfinal.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("scram nonce mismatch — replay protection failed"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }

    // 5. Verify proof.
    let auth_message =
        crate::auth::scram::auth_message(&client_first_bare, &server_first, &client_final_no_proof);
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
        let fail = encode_frame(&build_reply(
            cfinal.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail("invalid SCRAM proof"),
        )?);
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
    let ok = encode_frame(&build_reply(
        cfinal.correlation_id,
        MessageKind::AuthOk,
        ok_payload,
    )?);
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
    let (frame, _) =
        decode_frame(&buf).map_err(|e| io::Error::other(format!("decode frame: {e}")))?;
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
            build_dispatch_reply(frame.correlation_id, MessageKind::Result, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

fn run_query_with_params(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let (sql, params) = match decode_query_with_params(&frame.payload) {
        Ok(decoded) => decoded,
        Err(err) => return error_frame(frame.correlation_id, &err.to_string()),
    };
    let params = params
        .into_iter()
        .map(param_to_schema_value)
        .collect::<Vec<_>>();
    let parsed = match crate::storage::query::modes::parse_multi(&sql) {
        Ok(parsed) => parsed,
        Err(err) => return error_frame(frame.correlation_id, &err.to_string()),
    };
    let bound = match crate::storage::query::user_params::bind(&parsed, &params) {
        Ok(bound) => bound,
        Err(err) => return error_frame(frame.correlation_id, &err.to_string()),
    };
    match runtime.execute_query_expr(bound) {
        Ok(result) => {
            let is_mutation = matches!(result.statement_type, "insert" | "update" | "delete");
            if is_mutation {
                let post_lsn = runtime.cdc_current_lsn();
                if let Err(err) = runtime.enforce_commit_policy(post_lsn) {
                    return error_frame(frame.correlation_id, &err.to_string());
                }
            }
            let payload = serde_json::to_vec(
                &crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            )
            .unwrap_or_default();
            build_dispatch_reply(frame.correlation_id, MessageKind::Result, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

fn param_to_schema_value(value: RedWireParamValue) -> crate::storage::schema::Value {
    use crate::storage::schema::Value;
    match value {
        RedWireParamValue::Null => Value::Null,
        RedWireParamValue::Bool(value) => Value::Boolean(value),
        RedWireParamValue::Int(value) => Value::Integer(value),
        RedWireParamValue::Float(value) => Value::Float(value),
        RedWireParamValue::Text(value) => Value::Text(Arc::from(value.as_str())),
        RedWireParamValue::Bytes(value) => Value::Blob(value),
        RedWireParamValue::Vector(value) => Value::Vector(value),
        RedWireParamValue::Json(value) => Value::Json(value),
        RedWireParamValue::Timestamp(value) => Value::Timestamp(value),
        RedWireParamValue::Uuid(value) => Value::Uuid(value),
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
        None => {
            return error_frame(
                frame.correlation_id,
                "Insert: payload must be a JSON object",
            )
        }
    };
    let collection = match obj.get("collection").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return error_frame(frame.correlation_id, "Insert: missing 'collection' string"),
    };

    if let Some(rows) = obj.get("payloads").and_then(|x| x.as_array()) {
        let mut objects = Vec::with_capacity(rows.len());
        for entry in rows {
            objects.push(match entry.as_object() {
                Some(o) => o,
                None => {
                    return error_frame(
                        frame.correlation_id,
                        "Insert: each payload must be a JSON object",
                    )
                }
            });
        }

        if crate::rpc_stdio::should_bulk_insert_graph(runtime, collection, &objects) {
            return match crate::rpc_stdio::bulk_insert_graph(runtime, collection, &objects) {
                Ok(body) => {
                    let payload = serde_json::to_vec(&body).unwrap_or_default();
                    build_dispatch_reply(frame.correlation_id, MessageKind::BulkOk, payload)
                }
                Err(err) => error_frame(frame.correlation_id, &err.to_string()),
            };
        }

        let mut affected: u64 = 0;
        let mut ids = Vec::with_capacity(objects.len());
        for row in objects {
            let sql = crate::rpc_stdio::build_insert_sql(collection, row.iter());
            match runtime.execute_query(&sql) {
                Ok(qr) => {
                    affected += qr.affected_rows;
                    if let Some(id) = crate::rpc_stdio::insert_result_to_json(&qr).get("id") {
                        ids.push(id.clone());
                    }
                }
                Err(err) => return error_frame(frame.correlation_id, &err.to_string()),
            }
        }
        let mut out = crate::serde_json::Map::new();
        out.insert("affected".to_string(), JsonValue::Number(affected as f64));
        out.insert("ids".to_string(), JsonValue::Array(ids));
        let payload = serde_json::to_vec(&JsonValue::Object(out)).unwrap_or_default();
        return build_dispatch_reply(frame.correlation_id, MessageKind::BulkOk, payload);
    }

    let row = match obj.get("payload").and_then(|x| x.as_object()) {
        Some(o) => o,
        None => {
            return error_frame(
                frame.correlation_id,
                "Insert: missing 'payload' object or 'payloads' array",
            )
        }
    };
    let sql = crate::rpc_stdio::build_insert_sql(collection, row.iter());
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let body = crate::rpc_stdio::insert_result_to_json(&qr);
            let payload = serde_json::to_vec(&body).unwrap_or_default();
            build_dispatch_reply(frame.correlation_id, MessageKind::BulkOk, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

/// Build the primary-only topology payload embedded in HelloAck
/// (issue #167). Threads an anonymous auth context through
/// `TopologyAdvertiser::advertise` because the principal is not yet
/// known at HelloAck time — ADR 0008 §3 collapses anonymous to a
/// primary-only payload, which is exactly the bootstrap shape we
/// want here.
///
/// Returns `None` for non-primary roles or when the engine is not
/// running with replication enabled. Old clients that don't
/// understand the `topology` JSON key ignore it cleanly (ADR §4),
/// so the absent-vs-present distinction is benign.
fn build_topology_for_hello_ack(runtime: &RedDBRuntime) -> Option<reddb_wire::topology::Topology> {
    use crate::auth::middleware::AuthResult;
    use crate::replication::{LagConfig, TopologyAdvertiser};
    use reddb_wire::topology::Endpoint;

    let db = runtime.db();
    let primary_endpoint = Endpoint {
        addr: runtime.config_string("red.redwire.advertise_addr", ""),
        region: db.options().replication.region.clone(),
    };
    let (replicas, current_lsn, epoch) = match db.replication.as_ref() {
        Some(repl) => (
            repl.replica_snapshots(),
            repl.wal_buffer.current_lsn(),
            repl.topology_epoch(),
        ),
        None => (Vec::new(), 0u64, 0u64),
    };
    let lag = LagConfig::from_now();
    Some(TopologyAdvertiser::advertise(
        &replicas,
        &AuthResult::Anonymous,
        epoch,
        primary_endpoint,
        current_lsn,
        &lag,
    ))
}

fn error_frame(correlation_id: u64, msg: &str) -> Frame {
    // Error frames are guaranteed to fit (UTF-8 message bodies are
    // far smaller than MAX_FRAME_SIZE), so unwrapping the builder
    // here is sound — failure would mean the codebase generated a
    // 16 MiB error string, at which point we have a bigger problem.
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::Error)
        .payload(msg.as_bytes().to_vec())
        .build()
        .expect("error frame fits in MAX_FRAME_SIZE")
}

/// Single-frame reply via the [`FrameBuilder`] interface.
///
/// The dispatch loop needs builder-level enforcement (correlation-id
/// echo, MORE_FRAMES last-frame invariant, MAX_FRAME_SIZE check) on
/// every emitted frame; this helper folds the four-line builder
/// chain into one call so dispatch arms read like the table they
/// describe. `BuildError` surfaces as `io::Error::other` because the
/// dispatch loop already returns `io::Result`.
fn build_reply(correlation_id: u64, kind: MessageKind, payload: Vec<u8>) -> io::Result<Frame> {
    FrameBuilder::reply_to(correlation_id)
        .kind(kind)
        .payload(payload)
        .build()
        .map_err(|e| io::Error::other(format!("build {kind:?}: {e}")))
}

/// Variant for non-async dispatch helpers (`run_query`, `run_get`,
/// `run_delete`, `run_insert_dispatch`, `rewrap_handler_response`)
/// that return a `Frame` rather than `io::Result<Frame>`. Builder
/// failures degrade to a wire-level Error frame so the client always
/// gets a terminal response — the alternative (panic) would drop
/// the connection mid-reply.
fn build_dispatch_reply(correlation_id: u64, kind: MessageKind, payload: Vec<u8>) -> Frame {
    FrameBuilder::reply_to(correlation_id)
        .kind(kind)
        .payload(payload)
        .build()
        .unwrap_or_else(|e| error_frame(correlation_id, &e.to_string()))
}

/// Adapt a binary-fast-path handler response into a RedWire frame.
///
/// The fast-path handlers (`handle_bulk_insert_binary`,
/// `handle_query_binary`, etc) return their result already wrapped
/// in a 5-byte length-prefixed envelope (`[u32 length][u8 kind][body]`).
/// RedWire carries the same body verbatim — kinds 0x01..0x0F are
/// the same numeric values — so we strip the 5-byte envelope and
/// rewrap with a RedWire header using the response correlation id.
///
/// The payload `Vec<u8>` is moved into the new frame — no extra
/// allocation, no body copy — preserving max bulk-insert perf.
fn rewrap_handler_response(raw_bytes: &[u8], req: &Frame) -> Frame {
    if raw_bytes.len() < 5 {
        return error_frame(
            req.correlation_id,
            "fast-path handler returned a truncated frame",
        );
    }
    let kind_byte = raw_bytes[4];
    let kind = MessageKind::from_u8(kind_byte).unwrap_or(MessageKind::Error);
    let body = raw_bytes[5..].to_vec();
    build_dispatch_reply(req.correlation_id, kind, body)
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
            build_dispatch_reply(frame.correlation_id, MessageKind::Result, payload)
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
        None => {
            return error_frame(
                frame.correlation_id,
                "Delete: payload must be a JSON object",
            )
        }
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
            build_dispatch_reply(frame.correlation_id, MessageKind::DeleteOk, payload)
        }
        Err(err) => error_frame(frame.correlation_id, &err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::RedDBRuntime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn create_graph_collection(runtime: &RedDBRuntime, name: &str) {
        let db = runtime.db();
        db.store()
            .create_collection(name)
            .expect("create collection");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        db.save_collection_contract(crate::physical::CollectionContract {
            name: name.to_string(),
            declared_model: crate::catalog::CollectionModel::Graph,
            schema_mode: crate::catalog::SchemaMode::Dynamic,
            origin: crate::physical::ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            default_ttl_ms: None,
            vector_dimension: None,
            vector_metric: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: None,
            timestamps_enabled: false,
            context_index_enabled: false,
            metrics_raw_retention_ms: None,
            metrics_rollup_policies: Vec::new(),
            metrics_tenant_identity: None,
            metrics_namespace: None,
            append_only: false,
            subscriptions: Vec::new(),
            session_key: None,
            session_gap_ms: None,
            retention_duration_ms: None,
        })
        .expect("save graph contract");
    }

    #[test]
    fn magic_byte_is_0xfe() {
        assert_eq!(REDWIRE_MAGIC, 0xFE);
    }

    #[test]
    fn redwire_bulk_insert_graph_rows_returns_ids() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        create_graph_collection(&runtime, "network");

        let nodes = Frame::new(
            MessageKind::BulkInsert,
            7,
            br#"{"collection":"network","payloads":[{"label":"Host","name":"app"},{"label":"Host","name":"db"}]}"#.to_vec(),
        );
        let nodes_reply = run_insert_dispatch(&runtime, &nodes);
        assert_eq!(nodes_reply.kind, MessageKind::BulkOk);
        let node_body: JsonValue =
            serde_json::from_slice(&nodes_reply.payload).expect("nodes json");
        assert_eq!(
            node_body.get("affected").and_then(JsonValue::as_u64),
            Some(2)
        );
        let ids = node_body
            .get("ids")
            .and_then(JsonValue::as_array)
            .expect("node ids");
        assert_eq!(ids.len(), 2);

        let from = ids[0].as_u64().expect("from id");
        let to = ids[1].as_u64().expect("to id");
        let edges = Frame::new(
            MessageKind::BulkInsert,
            8,
            format!(
                r#"{{"collection":"network","payloads":[{{"label":"connects","from":{from},"to":{to},"role":"primary"}}]}}"#
            )
            .into_bytes(),
        );
        let edges_reply = run_insert_dispatch(&runtime, &edges);
        assert_eq!(edges_reply.kind, MessageKind::BulkOk);
        let edge_body: JsonValue =
            serde_json::from_slice(&edges_reply.payload).expect("edges json");
        assert_eq!(
            edge_body.get("affected").and_then(JsonValue::as_u64),
            Some(1)
        );
        assert_eq!(
            edge_body
                .get("ids")
                .and_then(JsonValue::as_array)
                .map(|ids| ids.len()),
            Some(1)
        );
    }

    /// Read a full RedWire frame off the client side of the duplex.
    async fn read_one_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Frame {
        let mut header = [0u8; FRAME_HEADER_SIZE];
        r.read_exact(&mut header).await.expect("read header");
        let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let mut buf = vec![0u8; length];
        buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
        if length > FRAME_HEADER_SIZE {
            r.read_exact(&mut buf[FRAME_HEADER_SIZE..])
                .await
                .expect("read body");
        }
        let (frame, _) = decode_frame(&buf).expect("decode");
        frame
    }

    fn stream_start_payload(coll: &str, cols: &[&str]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&(coll.len() as u16).to_le_bytes());
        p.extend_from_slice(coll.as_bytes());
        p.extend_from_slice(&(cols.len() as u16).to_le_bytes());
        for c in cols {
            p.extend_from_slice(&(c.len() as u16).to_le_bytes());
            p.extend_from_slice(c.as_bytes());
        }
        p
    }

    fn stream_rows_payload(rows: &[(i64, &str)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for (id, name) in rows {
            crate::wire::protocol::encode_value(
                &mut p,
                &crate::storage::schema::Value::Integer(*id),
            );
            crate::wire::protocol::encode_value(
                &mut p,
                &crate::storage::schema::Value::text(name.to_string()),
            );
        }
        p
    }

    /// Regression for #75: BulkStreamRows success must NOT emit a
    /// response frame. The legacy handler signals "no response" with
    /// an empty Vec; rewrapping that as a RedWire frame sent a stale
    /// ack back, and the next BulkStreamCommit response then carried
    /// the wrong correlation id (off-by-one) — clients failed with
    /// `wire: response correlation mismatch: sent N, got N-1`.
    #[tokio::test]
    async fn bulk_stream_rows_success_emits_no_response_frame() {
        // Server runtime + table the stream will land into.
        let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
        runtime
            .execute_query("CREATE TABLE target (id INT, name TEXT)")
            .expect("create table");

        // In-memory pipe — server side fed into handle_session, client
        // side speaks raw RedWire.
        let (server_io, mut client) = tokio::io::duplex(64 * 1024);

        let server_task = tokio::spawn(async move {
            let _ = handle_session(server_io, runtime, None, None).await;
        });

        // 1) Send the v2 minor-version byte the listener detector
        //    would have stripped before dispatching to handle_session.
        client.write_all(&[1u8]).await.expect("write minor");

        // 2) Hello — anonymous, since the server has no AuthStore.
        let hello_payload =
            br#"{"versions":[1],"auth_methods":["anonymous"],"features":0,"client_name":"test"}"#
                .to_vec();
        let hello = encode_frame(&Frame::new(MessageKind::Hello, 1, hello_payload));
        client.write_all(&hello).await.expect("write hello");

        // 3) Read HelloAck.
        let ack = read_one_frame(&mut client).await;
        assert_eq!(ack.kind, MessageKind::HelloAck);

        // 4) AuthResponse (anonymous needs no proof body).
        let authresp = encode_frame(&Frame::new(MessageKind::AuthResponse, 2, b"{}".to_vec()));
        client.write_all(&authresp).await.expect("write authresp");

        // 5) Read AuthOk.
        let auth_ok = read_one_frame(&mut client).await;
        assert_eq!(auth_ok.kind, MessageKind::AuthOk);

        // 6) BulkStreamStart — server sends a BulkStreamAck back.
        let start = encode_frame(&Frame::new(
            MessageKind::BulkStreamStart,
            3,
            stream_start_payload("target", &["id", "name"]),
        ));
        client.write_all(&start).await.expect("write start");
        let start_ack = read_one_frame(&mut client).await;
        assert_eq!(start_ack.kind, MessageKind::BulkStreamAck);
        assert_eq!(start_ack.correlation_id, 3);

        // 7) BulkStreamRows — success path MUST NOT emit a response.
        let rows = encode_frame(&Frame::new(
            MessageKind::BulkStreamRows,
            4,
            stream_rows_payload(&[(1, "a"), (2, "b")]),
        ));
        client.write_all(&rows).await.expect("write rows");

        // 8) BulkStreamCommit — server replies with BulkOk carrying
        //    correlation_id == 5. If the bug were still present, the
        //    next frame on the wire would be a BulkStreamAck with
        //    correlation_id 4 (the rewrapped empty success vec) and
        //    the assertion below would fail.
        let commit = encode_frame(&Frame::new(MessageKind::BulkStreamCommit, 5, vec![]));
        client.write_all(&commit).await.expect("write commit");

        let next = read_one_frame(&mut client).await;
        assert_eq!(
            next.kind,
            MessageKind::BulkOk,
            "expected BulkOk after commit; got {:?} — BulkStreamRows leaked an ack frame",
            next.kind
        );
        assert_eq!(
            next.correlation_id, 5,
            "commit response must carry the commit's correlation id"
        );

        // 9) Tear the session down cleanly.
        let bye = encode_frame(&Frame::new(MessageKind::Bye, 6, vec![]));
        client.write_all(&bye).await.expect("write bye");
        let _ = read_one_frame(&mut client).await; // drain Bye echo
        drop(client);
        let _ = server_task.await;
    }

    /// The error path for BulkStreamRows still has to produce a
    /// terminal frame so the client unblocks on the bad batch.
    #[tokio::test]
    async fn bulk_stream_rows_error_still_emits_error_frame() {
        let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
        let (server_io, mut client) = tokio::io::duplex(64 * 1024);

        let server_task = tokio::spawn(async move {
            let _ = handle_session(server_io, runtime, None, None).await;
        });

        client.write_all(&[1u8]).await.unwrap();
        let hello_payload =
            br#"{"versions":[1],"auth_methods":["anonymous"],"features":0}"#.to_vec();
        client
            .write_all(&encode_frame(&Frame::new(
                MessageKind::Hello,
                1,
                hello_payload,
            )))
            .await
            .unwrap();
        let _ack = read_one_frame(&mut client).await;
        client
            .write_all(&encode_frame(&Frame::new(
                MessageKind::AuthResponse,
                2,
                b"{}".to_vec(),
            )))
            .await
            .unwrap();
        let _auth_ok = read_one_frame(&mut client).await;

        // Send BulkStreamRows with no prior BulkStreamStart — the
        // legacy handler returns a non-empty Error frame, which the
        // session must forward.
        let rows = encode_frame(&Frame::new(
            MessageKind::BulkStreamRows,
            7,
            stream_rows_payload(&[(1, "a")]),
        ));
        client.write_all(&rows).await.unwrap();
        let resp = read_one_frame(&mut client).await;
        assert_eq!(resp.kind, MessageKind::Error);
        assert_eq!(resp.correlation_id, 7);

        drop(client);
        let _ = server_task.await;
    }
}
