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
use tokio::sync::{mpsc, Mutex as TokioMutex};

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
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
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

    // After handshake, split the socket so reads and writes are
    // independent: this is what makes RedWire multiplex (PRD #759
    // S3) — two concurrent output-stream workers can interleave
    // their chunks back to the client without contending on the
    // reader side. All outbound bytes are routed through an
    // unbounded mpsc; a drain task flushes them to the write half
    // under a mutex so chunk frames stay byte-atomic on the wire.
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(TokioMutex::new(writer));
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_drain = Arc::clone(&writer);
    tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            let mut w = writer_drain.lock().await;
            if w.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });

    // Per-connection output-stream registry (issue #762). Tracks
    // active stream workers so a `StreamCancel` for one stream_id
    // does not disturb the rest of the connection.
    let stream_registry = Arc::new(super::output_stream::StreamRegistry::new());

    // Per-connection input-stream registry (issue #764 / S5). Input
    // streams are driven inline from this reader loop — each
    // `StreamChunk` commits synchronously — so the registry is a plain
    // owned map rather than the `Arc<Mutex<…>>` the spawned output
    // workers share. Output and input streams are keyed by `stream_id`
    // in separate registries, so the two multiplex on one connection
    // without colliding (AC #2).
    let mut input_registry = super::input_stream::InputStreamRegistry::new();

    let mut buf = vec![0u8; FRAME_HEADER_SIZE];
    loop {
        // Read header.
        if let Err(err) = reader.read_exact(&mut buf[..FRAME_HEADER_SIZE]).await {
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
            reader
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
            queue_send(&out_tx, encode_frame(&err_frame))?;
            continue;
        }

        match frame.kind {
            MessageKind::Bye => {
                let bye = encode_frame(&build_reply(
                    frame.correlation_id,
                    MessageKind::Bye,
                    vec![],
                )?);
                let _ = out_tx.send(bye);
                return Ok(());
            }
            MessageKind::Ping => {
                let pong = encode_frame(&build_reply(
                    frame.correlation_id,
                    MessageKind::Pong,
                    vec![],
                )?);
                queue_send(&out_tx, pong)?;
            }
            MessageKind::Query => {
                let response = run_query(&runtime, &frame);
                queue_send(&out_tx, encode_frame(&response))?;
            }
            MessageKind::QueryWithParams => {
                let response = run_query_with_params(&runtime, &frame);
                queue_send(&out_tx, encode_frame(&response))?;
            }
            // BulkInsert handles both single-row and bulk shapes off
            // the same frame kind: payload `payload` = single,
            // payload `payloads` = array.
            MessageKind::BulkInsert => {
                let response = run_insert_dispatch(&runtime, &frame);
                queue_send(&out_tx, encode_frame(&response))?;
            }
            MessageKind::BulkInsertBinary => {
                let raw =
                    crate::wire::listener::handle_bulk_insert_binary(&runtime, &frame.payload);
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::BulkInsertPrevalidated => {
                let raw = crate::wire::listener::handle_bulk_insert_binary_prevalidated(
                    &runtime,
                    &frame.payload,
                );
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::QueryBinary => {
                let raw = crate::wire::listener::handle_query_binary(&runtime, &frame.payload);
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            // Streaming bulk insert (PG COPY equivalent).
            MessageKind::BulkStreamStart => {
                let raw =
                    crate::wire::listener::handle_stream_start(&frame.payload, &mut stream_session);
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::BulkStreamRows => {
                let raw = crate::wire::listener::handle_stream_rows(
                    &runtime,
                    &frame.payload,
                    &mut stream_session,
                );
                if !raw.is_empty() {
                    queue_send(
                        &out_tx,
                        encode_frame(&rewrap_handler_response(&raw, &frame)),
                    )?;
                }
            }
            MessageKind::BulkStreamCommit => {
                let raw =
                    crate::wire::listener::handle_stream_commit(&runtime, &mut stream_session);
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::Prepare => {
                let raw = crate::wire::listener::handle_prepare(
                    &runtime,
                    &frame.payload,
                    &mut prepared_stmts,
                );
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::ExecutePrepared => {
                let raw = crate::wire::listener::handle_execute_prepared(
                    &runtime,
                    &frame.payload,
                    &prepared_stmts,
                );
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_handler_response(&raw, &frame)),
                )?;
            }
            MessageKind::Get => {
                let response = run_get(&runtime, &frame);
                queue_send(&out_tx, encode_frame(&response))?;
            }
            MessageKind::Delete => {
                let response = run_delete(&runtime, &frame);
                queue_send(&out_tx, encode_frame(&response))?;
            }
            // Output-stream lifecycle (issue #762 / PRD #759 S3).
            //
            // OpenStream: parse payload, register the stream_id with
            // the per-connection registry, and spawn a worker that
            // emits OpenAck → StreamChunk* → StreamEnd through the
            // shared outbound channel. The dispatch loop returns to
            // reading immediately so concurrent streams interleave
            // on the wire (AC #2).
            MessageKind::OpenStream => {
                use super::output_stream as os;
                let frame_id = frame.correlation_id;
                let sid = frame.stream_id;

                // Input-stream open (issue #764 / S5). Distinguished by
                // `direction: "in"` in the payload; the output path
                // below (the default) keeps owning `sql`-bearing opens.
                // Input streams commit chunks inline in this loop, so
                // they are registered in the owned `input_registry`
                // rather than spawning a worker.
                if super::input_stream::open_stream_is_input(&frame.payload) {
                    use super::input_stream as is;
                    let req = match is::parse_open_input(&frame.payload) {
                        Ok(r) => r,
                        Err(e) => {
                            let err = is::build_input_stream_error_frame(
                                frame_id,
                                sid,
                                e.code(),
                                e.message(),
                                0,
                                0,
                            )?;
                            queue_send(&out_tx, encode_frame(&err))?;
                            continue;
                        }
                    };
                    let in_tx = runtime.connection_in_transaction(0);
                    let config = crate::server::output_stream::StreamConfig::load(&runtime);
                    let snapshot_lsn = runtime.cdc_current_lsn();
                    let clock = crate::server::output_stream::SystemClock;
                    let lease = match is::open_input_lease(config, snapshot_lsn, in_tx, &clock) {
                        Ok(l) => l,
                        Err(e) => {
                            let err = is::build_input_stream_error_frame(
                                frame_id,
                                sid,
                                e.code(),
                                e.message(),
                                0,
                                snapshot_lsn,
                            )?;
                            queue_send(&out_tx, encode_frame(&err))?;
                            continue;
                        }
                    };
                    let lease_id = lease.id;
                    let lease_snapshot = lease.snapshot_lsn;
                    let state = is::InputStreamState::new(lease, req.target, req.columns);
                    if let Err(e) = input_registry.register(sid, state) {
                        let err = is::build_input_stream_error_frame(
                            frame_id,
                            sid,
                            e.code(),
                            e.message(),
                            0,
                            snapshot_lsn,
                        )?;
                        queue_send(&out_tx, encode_frame(&err))?;
                        continue;
                    }
                    let ack = FrameBuilder::reply_to(frame_id)
                        .kind(MessageKind::OpenAck)
                        .stream_id(sid)
                        .payload(os::build_open_ack_payload(lease_id, lease_snapshot, false))
                        .build()
                        .map_err(|e| io::Error::other(format!("build OpenAck: {e}")))?;
                    queue_send(&out_tx, encode_frame(&ack))?;
                    continue;
                }

                let req = match os::parse_open_stream(&frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let err =
                            os::build_stream_error_frame(frame_id, sid, e.code(), e.message())?;
                        queue_send(&out_tx, encode_frame(&err))?;
                        continue;
                    }
                };
                let cancel_rx = match stream_registry.register(sid).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        let err =
                            os::build_stream_error_frame(frame_id, sid, e.code(), e.message())?;
                        queue_send(&out_tx, encode_frame(&err))?;
                        continue;
                    }
                };
                let runtime_ref = Arc::clone(&runtime);
                let registry_ref = Arc::clone(&stream_registry);
                let send = os::FrameTx::new(out_tx.clone());
                // RedWire today binds every connection to the
                // default tenant id (0); transactions are managed
                // per-connection via the task-local context that
                // HTTP also relies on. The S3 acceptance gate
                // mirrors S1's HTTP refusal.
                let in_tx = runtime.connection_in_transaction(0);
                tokio::spawn(async move {
                    os::run_output_stream(runtime_ref, frame_id, sid, req, in_tx, cancel_rx, send)
                        .await;
                    registry_ref.unregister(sid).await;
                });
            }
            // Live queue wait (issue #917 / PRD #915). Parse the open
            // request, then spawn a task that awaits the runtime's async
            // wait edge (parks on the registry's async wake head — no
            // blocking OS thread) and pushes a `QueueEventPush` the
            // instant a message becomes deliverable. The dispatch loop
            // returns to reading immediately so the wait multiplexes
            // with other frames on the connection.
            MessageKind::QueueWaitOpen => {
                use super::queue_wait as qw;
                let frame_id = frame.correlation_id;
                let sid = frame.stream_id;
                let req = match qw::parse_queue_wait_open(&frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let err =
                            qw::build_queue_wait_error_frame(frame_id, sid, e.code(), e.message())
                                .map_err(|e| {
                                    io::Error::other(format!("build queue-wait error: {e}"))
                                })?;
                        queue_send(&out_tx, encode_frame(&err))?;
                        continue;
                    }
                };
                let runtime_ref = Arc::clone(&runtime);
                let out = out_tx.clone();
                tokio::spawn(async move {
                    match runtime_ref
                        .redwire_queue_wait_json(
                            &req.queue,
                            req.group.as_deref(),
                            &req.consumer,
                            req.count,
                            req.wait_ms,
                        )
                        .await
                    {
                        Ok(messages) => {
                            // Happy path: push each delivered message.
                            // An empty Vec (deadline elapsed without a
                            // delivery) pushes nothing — the timeout
                            // surface is a later slice.
                            for message in messages {
                                match qw::build_event_push_frame(frame_id, sid, &message) {
                                    Ok(push) => {
                                        if queue_send(&out, encode_frame(&push)).is_err() {
                                            return;
                                        }
                                    }
                                    Err(_) => return,
                                }
                            }
                        }
                        Err(err) => {
                            if let Ok(ef) = qw::build_queue_wait_error_frame(
                                frame_id,
                                sid,
                                "queue_wait_failed",
                                &err.to_string(),
                            ) {
                                let _ = queue_send(&out, encode_frame(&ef));
                            }
                        }
                    }
                });
            }
            // Input-stream chunk (issue #764 / S5). A `StreamChunk`
            // from the client carries a chunk of rows for an open
            // input stream. Each chunk commits synchronously and
            // atomically; success is silent (await the next chunk), a
            // `terminal: true` chunk closes the stream with a
            // `StreamEnd`, and a commit failure emits one `StreamError`
            // (carrying `recoverable_rid`) after which no further
            // frames are produced for this `stream_id` (AC #3).
            MessageKind::StreamChunk => {
                use super::input_stream as is;
                use crate::server::output_stream::{Clock, SystemClock};
                let frame_id = frame.correlation_id;
                let sid = frame.stream_id;
                if !input_registry.contains(sid) {
                    // No input stream for this id — protocol violation,
                    // surfaced as StreamError rather than a drop.
                    let err = is::build_input_stream_error_frame(
                        frame_id,
                        sid,
                        "unknown_stream",
                        "no active input stream for this stream_id",
                        0,
                        0,
                    )?;
                    queue_send(&out_tx, encode_frame(&err))?;
                    continue;
                }
                let chunk = match is::parse_input_chunk(&frame.payload) {
                    Ok(c) => c,
                    Err(e) => {
                        let state = input_registry
                            .remove(sid)
                            .expect("stream presence checked above");
                        let err = is::build_input_stream_error_frame(
                            frame_id,
                            sid,
                            e.code(),
                            e.message(),
                            state.chunk_count,
                            state.committed_rid,
                        )?;
                        queue_send(&out_tx, encode_frame(&err))?;
                        continue;
                    }
                };
                let commit_result = {
                    let state = input_registry
                        .get_mut(sid)
                        .expect("stream presence checked above");
                    if state.lease.snapshot_expired(SystemClock.now_ms()) {
                        Err((
                            "snapshot_expired".to_string(),
                            "stream snapshot pin TTL elapsed".to_string(),
                        ))
                    } else {
                        state.commit_chunk(&runtime, &chunk.rows)
                    }
                };
                match commit_result {
                    Err((code, message)) => {
                        let state = input_registry
                            .remove(sid)
                            .expect("stream presence checked above");
                        let err = is::build_input_stream_error_frame(
                            frame_id,
                            sid,
                            &code,
                            &message,
                            state.chunk_count,
                            state.committed_rid,
                        )?;
                        queue_send(&out_tx, encode_frame(&err))?;
                    }
                    Ok(()) => {
                        if chunk.terminal {
                            let state = input_registry
                                .remove(sid)
                                .expect("stream presence checked above");
                            let end = is::build_input_stream_end_frame(
                                frame_id,
                                sid,
                                state.row_count,
                                state.chunk_count,
                                state.committed_rid,
                                state.snapshot_lsn,
                                false,
                            )?;
                            queue_send(&out_tx, encode_frame(&end))?;
                        }
                    }
                }
            }
            MessageKind::StreamCancel => {
                use super::input_stream as is;
                use super::output_stream as os;
                let sid = frame.stream_id;
                if stream_registry.cancel(sid).await {
                    // Output stream cancelled — its worker emits the
                    // terminal StreamEnd(cancelled=true) itself.
                } else if let Some(state) = input_registry.remove(sid) {
                    // AC #4 — input-stream cancel: the in-flight (not
                    // yet committed) chunk is discarded by dropping the
                    // state; prior per-chunk commits stay durable. Emit
                    // a terminal StreamEnd with cancelled=true so the
                    // client can drop its bookkeeping.
                    let end = is::build_input_stream_end_frame(
                        frame.correlation_id,
                        sid,
                        state.row_count,
                        state.chunk_count,
                        state.committed_rid,
                        state.snapshot_lsn,
                        true,
                    )?;
                    queue_send(&out_tx, encode_frame(&end))?;
                } else {
                    // AC #6: protocol violation surfaces as a
                    // StreamError envelope, not a connection drop.
                    let err = os::build_stream_error_frame(
                        frame.correlation_id,
                        sid,
                        "unknown_stream",
                        "no active stream for this stream_id",
                    )?;
                    queue_send(&out_tx, encode_frame(&err))?;
                }
            }
            other => {
                let err_frame = FrameBuilder::reply_to(frame.correlation_id)
                    .kind(MessageKind::Error)
                    .payload(format!("redwire: cannot dispatch {other:?} yet").into_bytes())
                    .build()
                    .map_err(|e| io::Error::other(format!("build Error frame: {e}")))?;
                queue_send(&out_tx, encode_frame(&err_frame))?;
            }
        }
    }
}

#[inline]
fn queue_send(out_tx: &mpsc::UnboundedSender<Vec<u8>>, bytes: Vec<u8>) -> io::Result<()> {
    out_tx
        .send(bytes)
        .map_err(|_| io::Error::other("redwire: write channel closed"))
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

/// Insert dispatch — handles single-row, bulk, and the analytics
/// batch shape off the same `BulkInsert` (0x04) frame:
///   - `{ "collection": "...", "payload": {...} }` → single insert
///   - `{ "collection": "...", "payloads": [...] }` → bulk insert
///   - `{ "collection": "...", "payloads": [...], "idempotency_key": "...",
///       "batch": true? }` → analytics `BatchInsertEndpoint`
///     (issue #587) — all-or-nothing commit with
///     `AnalyticsSchemaRegistry` validation up front and replay served
///     from the process-wide cache shared with the HTTP (#582) and
///     gRPC (#585) mirrors. Either an `idempotency_key` OR `batch:
///     true` flips the dispatch — the literal idempotency key in the
///     frame is the canonical signal in the brief, the boolean lets a
///     client opt into the validation semantics without committing to
///     a cache window.
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

    // Analytics batch-insert path (issue #587). Either field flips the
    // dispatch — the brief carries `idempotency_key` as the canonical
    // signal; the optional `batch: true` boolean exists for callers
    // that want the validation contract without committing to a
    // replay window.
    let idempotency_key = obj.get("idempotency_key").and_then(|x| x.as_str());
    let batch_flag = obj.get("batch").and_then(|x| x.as_bool()).unwrap_or(false);
    if idempotency_key.is_some() || batch_flag {
        let items = match obj.get("payloads").and_then(|x| x.as_array()) {
            Some(rows) => rows,
            None => {
                return error_frame(
                    frame.correlation_id,
                    "BatchInsert: missing 'payloads' array",
                )
            }
        };
        let outcome = crate::server::handlers_entity::process_batch_insert(
            runtime,
            collection,
            items,
            idempotency_key,
        );
        // Mirror the HTTP transport's status convention: 2xx → BulkOk,
        // everything else → Error frame (the body carries the typed
        // code/row_index envelope so the client can decode it without
        // an out-of-band header).
        let kind = if (200..300).contains(&outcome.status) {
            MessageKind::BulkOk
        } else {
            MessageKind::Error
        };
        return build_dispatch_reply(frame.correlation_id, kind, outcome.body);
    }

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
            analytics_config: Vec::new(),
            session_key: None,
            session_gap_ms: None,
            retention_duration_ms: None,
            analytical_storage: None,
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

    // ── Issue #587 — BatchInsertEndpoint RedWire mirror ──────────────
    //
    // The brief carries the rows + idempotency key in the existing
    // `BulkInsert` (0x04) frame: the presence of `idempotency_key` in
    // the JSON payload flips the dispatch onto the analytics batch
    // path (all-or-nothing commit, AnalyticsSchemaRegistry validation,
    // process-wide cache shared with HTTP #582 and gRPC #585). Each
    // test below maps to one acceptance bullet.

    /// Bullet 1 — wire form: `BulkInsert` payload with
    /// `idempotency_key` routes to the batch path; success returns a
    /// `BulkOk` frame carrying `{"ok":true,"count":N}`. Bullet 5 —
    /// every row commits in submission order (we read them back and
    /// assert ascending storage order matches insertion order).
    #[test]
    fn redwire_batch_insert_happy_path_returns_bulkok_with_count() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_ok (id INTEGER, name TEXT)")
            .expect("create table");

        let frame = Frame::new(
            MessageKind::BulkInsert,
            100,
            br#"{
                "collection":"events_587_ok",
                "idempotency_key":"k-ok",
                "payloads":[
                    {"fields":{"id":1,"name":"a"}},
                    {"fields":{"id":2,"name":"b"}},
                    {"fields":{"id":3,"name":"c"}}
                ]
            }"#
            .to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &frame);
        assert_eq!(
            reply.kind,
            MessageKind::BulkOk,
            "body={:?}",
            String::from_utf8_lossy(&reply.payload)
        );
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("ok body json");
        assert_eq!(body.get("ok").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(body.get("count").and_then(JsonValue::as_u64), Some(3));

        // Submission-order commit — every row landed and the scan can
        // see them all. (CDC ordering is a property of
        // `create_rows_batch`, which the shared
        // `process_batch_insert` re-uses; we pin the user-observable
        // surface here.)
        let qr = runtime
            .execute_query("SELECT name FROM events_587_ok ORDER BY id ASC")
            .expect("scan");
        let names: Vec<String> = qr
            .result
            .records
            .iter()
            .filter_map(|record| match record.get("name") {
                Some(crate::storage::schema::Value::Text(s)) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    /// Bullet 3 — row K's failure rolls back the whole batch; the
    /// reply is an `Error` frame whose JSON body carries the failing
    /// `row_index` so the client can pinpoint the broken row without
    /// re-uploading.
    #[test]
    fn redwire_batch_insert_row_failure_rolls_back_with_row_index() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_rollback (id INTEGER, name TEXT)")
            .expect("create table");

        // Row index 1 omits the required `fields` envelope — the parse
        // step rejects before any commit fires.
        let frame = Frame::new(
            MessageKind::BulkInsert,
            101,
            br#"{
                "collection":"events_587_rollback",
                "idempotency_key":"k-rollback",
                "payloads":[
                    {"fields":{"id":1,"name":"a"}},
                    {"not_fields":{"id":2}},
                    {"fields":{"id":3,"name":"c"}}
                ]
            }"#
            .to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &frame);
        assert_eq!(reply.kind, MessageKind::Error);
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("err body json");
        assert_eq!(body.get("ok").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(
            body.get("code").and_then(JsonValue::as_str),
            Some("RowParseFailure")
        );
        assert_eq!(body.get("row_index").and_then(JsonValue::as_u64), Some(1));

        // Storage untouched — row 0 was never committed even though
        // it would have parsed cleanly on its own.
        let qr = runtime
            .execute_query("SELECT name FROM events_587_rollback")
            .expect("scan");
        assert!(
            qr.result.records.is_empty(),
            "row 0 leaked despite row 1 rejection: {} rows present",
            qr.result.records.len()
        );
    }

    /// Bullet 2 — `idempotency_key` carried in the frame; the
    /// process-wide cache (shared with HTTP slice 4) replays a
    /// previous success byte-for-byte even when the retry's body
    /// differs from the original. The HTTP slice 4 already pins the
    /// cross-call behaviour at its boundary; this test pins the
    /// RedWire boundary plus the cross-transport sharing (a retry on
    /// the same key via HTTP returns the body RedWire just produced).
    #[test]
    fn redwire_batch_insert_idempotency_key_replays_cached_result() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_dedup (id INTEGER, name TEXT)")
            .expect("create table");

        // Use a process-unique key so this test doesn't trample
        // (or get trampled by) the HTTP-side dedup test that shares
        // the global cache.
        let key = format!(
            "redwire-587-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let frame1 = Frame::new(
            MessageKind::BulkInsert,
            200,
            format!(
                r#"{{
                    "collection":"events_587_dedup",
                    "idempotency_key":"{key}",
                    "payloads":[{{"fields":{{"id":1,"name":"first"}}}}]
                }}"#
            )
            .into_bytes(),
        );
        let reply1 = run_insert_dispatch(&runtime, &frame1);
        assert_eq!(reply1.kind, MessageKind::BulkOk);
        let body1 = reply1.payload.clone();

        // Replay with the same key + DIFFERENT body — the cache
        // returns the original bytes verbatim and the second row is
        // not committed.
        let frame2 = Frame::new(
            MessageKind::BulkInsert,
            201,
            format!(
                r#"{{
                    "collection":"events_587_dedup",
                    "idempotency_key":"{key}",
                    "payloads":[{{"fields":{{"id":2,"name":"second"}}}}]
                }}"#
            )
            .into_bytes(),
        );
        let reply2 = run_insert_dispatch(&runtime, &frame2);
        assert_eq!(reply2.kind, MessageKind::BulkOk);
        assert_eq!(
            reply2.payload, body1,
            "replay must return cached body byte-for-byte"
        );

        let qr = runtime
            .execute_query("SELECT name FROM events_587_dedup")
            .expect("scan");
        assert_eq!(
            qr.result.records.len(),
            1,
            "replay re-executed and committed the second row"
        );
    }

    /// Bullet 2 (cont.) — the cache is *shared with HTTP slice 4*: a
    /// RedWire submission populates the cache, and a same-key HTTP
    /// retry returns the cached body verbatim.
    #[test]
    fn redwire_batch_insert_cache_shared_with_http_transport() {
        use crate::runtime::batch_insert::global_cache;

        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_shared (id INTEGER, name TEXT)")
            .expect("create table");

        let key = format!(
            "shared-cache-587-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let frame = Frame::new(
            MessageKind::BulkInsert,
            300,
            format!(
                r#"{{
                    "collection":"events_587_shared",
                    "idempotency_key":"{key}",
                    "payloads":[{{"fields":{{"id":1,"name":"x"}}}}]
                }}"#
            )
            .into_bytes(),
        );
        let reply = run_insert_dispatch(&runtime, &frame);
        assert_eq!(reply.kind, MessageKind::BulkOk);

        // Look the entry up directly via the process-wide cache that
        // both HTTP and RedWire share. A hit here is the entire
        // "shared with HTTP slice 4" contract.
        let hit = global_cache()
            .lookup("events_587_shared", &key, std::time::Instant::now())
            .expect("shared cache must serve the RedWire write to HTTP");
        assert_eq!(hit.status, 200);
        assert_eq!(hit.body, reply.payload);
    }

    /// Bullet 4 — schema-validation failure mirrors the other
    /// transports: a row that the `AnalyticsSchemaRegistry` rejects
    /// surfaces as `RowSchemaRejected` with the offending `row_index`,
    /// and the batch leaves the collection untouched.
    #[test]
    fn redwire_batch_insert_schema_validation_rejects_unknown_field() {
        use crate::runtime::analytics_schema_registry as reg;

        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_schema (event_name TEXT, payload TEXT)")
            .expect("create table");

        let schema =
            r#"{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}"#;
        reg::register(runtime.db().store().as_ref(), "click_587", schema).expect("register schema");

        let frame = Frame::new(
            MessageKind::BulkInsert,
            400,
            br#"{
                "collection":"events_587_schema",
                "idempotency_key":"k-schema",
                "payloads":[
                    {"fields":{"event_name":"click_587","payload":"{\"url\":\"/a\"}"}},
                    {"fields":{"event_name":"click_587","payload":"{\"url\":\"/b\",\"extra\":1}"}}
                ]
            }"#
            .to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &frame);
        assert_eq!(reply.kind, MessageKind::Error);
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("err body json");
        assert_eq!(
            body.get("code").and_then(JsonValue::as_str),
            Some("RowSchemaRejected")
        );
        assert_eq!(body.get("row_index").and_then(JsonValue::as_u64), Some(1));

        let qr = runtime
            .execute_query("SELECT event_name FROM events_587_schema")
            .expect("scan");
        assert!(
            qr.result.records.is_empty(),
            "row 0 leaked despite row 1 schema rejection"
        );
    }

    /// Bullet 4 (cont.) — oversize fails with `BatchTooLarge` and a
    /// 413-equivalent status; the storage is never touched.
    ///
    /// Build one row past the default ceiling rather than mutating
    /// `RED_BATCH_MAX_ROWS`. The env var is process-wide and the
    /// `cargo test` runner schedules tests in parallel; a `set_var`
    /// here leaks into sibling tests in this crate (e.g. the
    /// row-failure case sees its 3-row batch flagged as oversize).
    /// The HTTP slice 4 test takes the same "build past the default"
    /// route for the same reason.
    #[test]
    fn redwire_batch_insert_oversize_returns_error_before_storage() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE events_587_oversize (id INTEGER, name TEXT)")
            .expect("create table");

        // Default `red.batch.max_rows = 10_000`; submit one more.
        let max = 10_000usize;
        let mut payloads = String::with_capacity(max * 32);
        payloads.push('[');
        for i in 0..(max + 1) {
            if i > 0 {
                payloads.push(',');
            }
            payloads.push_str(&format!(r#"{{"fields":{{"id":{i},"name":"x"}}}}"#));
        }
        payloads.push(']');
        let frame_body = format!(
            r#"{{"collection":"events_587_oversize","idempotency_key":"k-oversize-587","payloads":{payloads}}}"#
        );
        let frame = Frame::new(MessageKind::BulkInsert, 500, frame_body.into_bytes());
        let reply = run_insert_dispatch(&runtime, &frame);

        assert_eq!(reply.kind, MessageKind::Error);
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("err body json");
        assert_eq!(
            body.get("code").and_then(JsonValue::as_str),
            Some("BatchTooLarge")
        );
        let qr = runtime
            .execute_query("SELECT name FROM events_587_oversize")
            .expect("scan");
        assert!(
            qr.result.records.is_empty(),
            "oversize batch leaked rows into storage"
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
