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

use crate::application::{OperationContext, OperationContextFactory, OperationContextInput};
use crate::auth::store::AuthStore;
use crate::auth::Role;
use crate::runtime::query_request::{
    ParamValue, PreparedRegistry, QueryRequest, QueryRequestExecutor,
};
use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};
use reddb_wire::query_with_params::{
    decode_query_with_params_request, ParamValue as RedWireParamValue, FEATURE_PARAMS,
};
use reddb_wire::redwire::operations::{
    decode_delete_payload, decode_get_payload, decode_insert_dispatch_payload,
    encode_bulk_ok_payload_from_json_id_literals, encode_delete_ok_payload,
    encode_get_result_payload,
};
use reddb_wire::redwire::scram::{ScramServerHandshake, ScramServerInput, ScramServerOutput};

use super::auth::{build_auth_ok, pick_auth_method, validate_auth_response, AuthOutcome};
use super::validate_minor_version;
use reddb_wire::redwire::handshake::{
    build_auth_fail_payload, build_auth_ok_frame_from_payload, build_hello_ack_frame,
    expect_auth_response_payload, parse_auth_response_oauth_jwt, Hello,
};
use reddb_wire::redwire::{
    build_dispatch_reply_frame, build_error_frame_lossy, build_reply_frame,
    choose_hello_minor_version, decode_frame, encode_frame, read_frame_async,
    rewrap_length_prefixed_handler_response, Frame, MessageDirection, MessageKind, REDWIRE_MAGIC,
};

#[derive(Debug)]
struct AuthedSession {
    username: String,
    role: Role,
    tenant: Option<String>,
    session_id: String,
}

struct RedWireAuthenticatedSessionPolicy;

impl crate::server::route_catalog::CommandPolicyEngine for RedWireAuthenticatedSessionPolicy {
    fn allows(
        &self,
        _ctx: &OperationContext,
        _command: &crate::server::route_catalog::CommandSpec,
    ) -> bool {
        // Reaching the frame loop means the RedWire handshake accepted this
        // session (including explicitly configured anonymous sessions). The
        // catalog authorizer still owns command lookup and the mandatory
        // authorization consult, while this adapter preserves the handshake's
        // existing credential decision.
        true
    }
}

fn redwire_operation_context(session: &AuthedSession, correlation_id: u64) -> OperationContext {
    OperationContextFactory::build(OperationContextInput {
        request_id: Some(format!("redwire-{}-{correlation_id}", session.session_id)),
        principal: Some(session.username.clone()),
        tenant: session.tenant.clone(),
        ..OperationContextInput::default()
    })
}

fn authorize_redwire_frame<P: crate::server::route_catalog::CommandPolicyEngine + ?Sized>(
    ctx: &OperationContext,
    kind: MessageKind,
    policy: &P,
) -> Result<(), crate::server::route_catalog::CommandAuthorizationError> {
    let Some(command_id) = redwire_command_id(kind) else {
        return Ok(());
    };
    crate::server::route_catalog::CommandAuthorizer::new(
        crate::server::discovered_route_catalog(),
        policy,
    )
    .authorize(ctx, command_id)
}

/// Canonical command id for every RedWire frame kind this server dispatches.
/// `Hello` and `AuthResponse` are handled by the handshake; the other bound
/// kinds have real arms in `handle_session`. `None` means the wire vocabulary
/// declares the kind but this server does not dispatch it.
pub(crate) const fn redwire_command_id(kind: MessageKind) -> Option<&'static str> {
    match kind {
        MessageKind::Hello
        | MessageKind::HelloAck
        | MessageKind::AuthRequest
        | MessageKind::AuthResponse
        | MessageKind::AuthOk
        | MessageKind::AuthFail => Some("auth.login"),

        MessageKind::Bye | MessageKind::Ping | MessageKind::Pong => Some("health.live"),

        MessageKind::Query
        | MessageKind::Result
        | MessageKind::Error
        | MessageKind::QueryBinary
        | MessageKind::Prepare
        | MessageKind::PreparedOk
        | MessageKind::ExecutePrepared
        | MessageKind::QueryWithParams
        | MessageKind::QueueWaitOpen
        | MessageKind::QueueEventPush
        | MessageKind::QueueWaitTimeout
        | MessageKind::MovedRedirect
        | MessageKind::Compress => Some("query.execute"),

        MessageKind::BulkInsert
        | MessageKind::BulkOk
        | MessageKind::BulkInsertBinary
        | MessageKind::BulkInsertPrevalidated
        | MessageKind::BulkStreamStart
        | MessageKind::BulkStreamRows
        | MessageKind::BulkStreamCommit
        | MessageKind::BulkStreamAck => Some("collections.batch.insert"),

        MessageKind::Get => Some("collections.entities.get"),
        MessageKind::Delete | MessageKind::DeleteOk => Some("collections.entities.delete"),

        MessageKind::OpenStream
        | MessageKind::OpenAck
        | MessageKind::RowDescription
        | MessageKind::StreamEnd => Some("streams.query.output"),
        MessageKind::StreamChunk => Some("streams.input"),
        MessageKind::Cancel | MessageKind::StreamCancel | MessageKind::StreamError => {
            Some("streams.query.cancel")
        }

        MessageKind::SetSession
        | MessageKind::Notice
        | MessageKind::VectorSearch
        | MessageKind::GraphTraverse => None,
    }
}

struct RedWireExecutionContextGuard {
    previous_tenant: Option<String>,
    previous_identity: Option<(String, Role)>,
}

impl RedWireExecutionContextGuard {
    fn install(ctx: &OperationContext, role: Role) -> Self {
        let previous_tenant = crate::runtime::execution_context::current_tenant();
        let previous_identity = crate::runtime::execution_context::current_auth_identity();
        match &ctx.tenant {
            Some(tenant) => crate::runtime::execution_context::set_current_tenant(tenant.clone()),
            None => crate::runtime::execution_context::clear_current_tenant(),
        }
        // Anonymous sessions exist only when server auth is disabled (the
        // handshake refuses anonymous otherwise) and historically ran with
        // NO installed identity — enforcement off. Installing the handshake's
        // `Read` role here would reject every anonymous write (issue #2149
        // review). The tenant install above still applies for RLS.
        if ctx.audit_principal == "anonymous" {
            crate::runtime::execution_context::clear_current_auth_identity();
        } else {
            crate::runtime::execution_context::set_current_auth_identity(
                ctx.audit_principal.clone(),
                role,
            );
        }
        Self {
            previous_tenant,
            previous_identity,
        }
    }
}

impl Drop for RedWireExecutionContextGuard {
    fn drop(&mut self) {
        match self.previous_identity.take() {
            Some((username, role)) => {
                crate::runtime::execution_context::set_current_auth_identity(username, role)
            }
            None => crate::runtime::execution_context::clear_current_auth_identity(),
        }
        match self.previous_tenant.take() {
            Some(tenant) => crate::runtime::execution_context::set_current_tenant(tenant),
            None => crate::runtime::execution_context::clear_current_tenant(),
        }
    }
}

pub(super) fn execute_with_redwire_context<T>(
    ctx: &OperationContext,
    role: Role,
    execute: impl FnOnce() -> T,
) -> T {
    let _guard = RedWireExecutionContextGuard::install(ctx, role);
    execute()
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
    let session = session.unwrap();

    // Per-connection state for prepared statements + streaming
    // bulk inserts. Owned by the session; dropped on disconnect. The
    // prepared registry inside `prepared_stmts` is also the Request
    // module's execution scope for this connection's queries.
    let mut stream_session: Option<crate::wire::listener::BulkStreamSession> = None;
    let mut prepared_stmts = crate::wire::listener::PreparedStatements::default();

    // After handshake, split the socket so reads and writes are
    // independent: this is what makes RedWire multiplex (PRD #759
    // S3) — two concurrent output-stream workers can interleave
    // their chunks back to the client without contending on the
    // reader side. All outbound bytes are routed through an
    // unbounded mpsc; a drain task flushes them to the write half
    // under a mutex so chunk frames stay byte-atomic on the wire.
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(TokioMutex::new(writer));
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_FRAMES);
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

    // In-flight live queue-wait tasks (issue #920). Unlike the
    // output-stream workers — which notice a closed connection the
    // instant they try to push and self-terminate — a queue-wait task
    // parks on the registry's async wake head and would otherwise linger
    // until its `wait_ms` deadline after the client disconnects, holding
    // a registry slot reference and a tokio worker the whole time. Owning
    // the tasks in a `JoinSet` scoped to this connection fixes that: when
    // the frame loop returns (Bye / EOF / I/O error), the set is dropped
    // and every still-parked wait is aborted, dropping its waiter and so
    // releasing the slot reference promptly (AC #1, AC #3). The
    // registry's own `cancel_all` still drives the server-shutdown path
    // (AC #2/#4) independently of this connection-scoped abort.
    let mut queue_wait_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        // Reap finished wait tasks so the set does not accumulate joined
        // handles over a long-lived connection. Non-blocking — only
        // already-complete tasks are drained.
        while queue_wait_tasks.try_join_next().is_some() {}

        let frame = match read_frame_async(&mut reader).await {
            Ok(frame) => frame,
            Err(reddb_wire::redwire::RedWireIoError::Io(err))
                if err.kind() == io::ErrorKind::UnexpectedEof =>
            {
                return Ok(())
            }
            Err(err) => return Err(redwire_io_err(err)),
        };

        let operation_context = redwire_operation_context(&session, frame.correlation_id);
        if let Err(error) = authorize_redwire_frame(
            &operation_context,
            frame.kind,
            &RedWireAuthenticatedSessionPolicy,
        ) {
            let err_frame = build_error_frame_lossy(frame.correlation_id, &error.to_string());
            queue_send(&out_tx, encode_frame(&err_frame)).await?;
            continue;
        }

        // Catalog-driven direction gate: server-only kinds (PreparedOk,
        // AuthOk/Fail, BulkOk, …) must never arrive *from* a client.
        // The catalog (`MessageKind::direction`) is the single source
        // of truth — see `frame.rs::catalog_tests::direction_matrix_is_pinned`.
        if frame.kind.direction() == MessageDirection::ServerToClient {
            let err_frame = build_error_frame_lossy(
                frame.correlation_id,
                &format!("redwire: {:?} is server-only", frame.kind),
            );
            queue_send(&out_tx, encode_frame(&err_frame)).await?;
            continue;
        }

        match frame.kind {
            MessageKind::Bye => {
                let bye = encode_frame(&reply_frame_or_io_error(
                    frame.correlation_id,
                    MessageKind::Bye,
                    vec![],
                )?);
                let _ = out_tx.send(bye).await;
                return Ok(());
            }
            MessageKind::Ping => {
                let pong = encode_frame(&reply_frame_or_io_error(
                    frame.correlation_id,
                    MessageKind::Pong,
                    vec![],
                )?);
                queue_send(&out_tx, pong).await?;
            }
            MessageKind::Query => {
                let response =
                    execute_with_redwire_context(&operation_context, session.role, || {
                        run_query(&runtime, prepared_stmts.registry(), &frame)
                    });
                queue_send(&out_tx, encode_frame(&response)).await?;
            }
            MessageKind::QueryWithParams => {
                let response =
                    execute_with_redwire_context(&operation_context, session.role, || {
                        run_query_with_params(&runtime, prepared_stmts.registry(), &frame)
                    });
                queue_send(&out_tx, encode_frame(&response)).await?;
            }
            // BulkInsert handles both single-row and bulk shapes off
            // the same frame kind: payload `payload` = single,
            // payload `payloads` = array.
            MessageKind::BulkInsert => {
                let response =
                    execute_with_redwire_context(&operation_context, session.role, || {
                        run_insert_dispatch(&runtime, &frame)
                    });
                queue_send(&out_tx, encode_frame(&response)).await?;
            }
            MessageKind::BulkInsertBinary => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_bulk_insert_binary(&runtime, &frame.payload)
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::BulkInsertPrevalidated => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_bulk_insert_binary_prevalidated(
                        &runtime,
                        &frame.payload,
                    )
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::QueryBinary => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_query_binary(&runtime, &frame.payload)
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            // Streaming bulk insert (PG COPY equivalent).
            MessageKind::BulkStreamStart => {
                let raw =
                    crate::wire::listener::handle_stream_start(&frame.payload, &mut stream_session);
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::BulkStreamRows => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_stream_rows(
                        &runtime,
                        &frame.payload,
                        &mut stream_session,
                    )
                });
                if !raw.is_empty() {
                    queue_send(
                        &out_tx,
                        encode_frame(&rewrap_length_prefixed_handler_response(
                            &raw,
                            frame.correlation_id,
                        )),
                    )
                    .await?;
                }
            }
            MessageKind::BulkStreamCommit => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_stream_commit(&runtime, &mut stream_session)
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::Prepare => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_prepare(
                        &runtime,
                        &frame.payload,
                        &mut prepared_stmts,
                    )
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::ExecutePrepared => {
                let raw = execute_with_redwire_context(&operation_context, session.role, || {
                    crate::wire::listener::handle_execute_prepared(
                        &runtime,
                        &frame.payload,
                        &prepared_stmts,
                    )
                });
                queue_send(
                    &out_tx,
                    encode_frame(&rewrap_length_prefixed_handler_response(
                        &raw,
                        frame.correlation_id,
                    )),
                )
                .await?;
            }
            MessageKind::Get => {
                let response =
                    execute_with_redwire_context(&operation_context, session.role, || {
                        run_get(&runtime, &frame)
                    });
                queue_send(&out_tx, encode_frame(&response)).await?;
            }
            MessageKind::Delete => {
                let response =
                    execute_with_redwire_context(&operation_context, session.role, || {
                        run_delete(&runtime, &frame)
                    });
                queue_send(&out_tx, encode_frame(&response)).await?;
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
                            queue_send(&out_tx, encode_frame(&err)).await?;
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
                            queue_send(&out_tx, encode_frame(&err)).await?;
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
                        queue_send(&out_tx, encode_frame(&err)).await?;
                        continue;
                    }
                    let ack =
                        os::build_open_ack_frame(frame_id, sid, lease_id, lease_snapshot, false)
                            .map_err(|e| io::Error::other(format!("build OpenAck: {e}")))?;
                    queue_send(&out_tx, encode_frame(&ack)).await?;
                    continue;
                }

                let req = match os::parse_open_stream(&frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let err =
                            os::build_stream_error_frame(frame_id, sid, e.code(), e.message())?;
                        queue_send(&out_tx, encode_frame(&err)).await?;
                        continue;
                    }
                };
                let cancel_rx = match stream_registry.register(sid).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        let err =
                            os::build_stream_error_frame(frame_id, sid, e.code(), e.message())?;
                        queue_send(&out_tx, encode_frame(&err)).await?;
                        continue;
                    }
                };
                let runtime_ref = Arc::clone(&runtime);
                let registry_ref = Arc::clone(&stream_registry);
                let send = os::FrameTx::new(out_tx.clone());
                let stream_context = operation_context.clone();
                let stream_role = session.role;
                // Transactions are still managed per connection using the
                // default connection id. The stream's authenticated tenant
                // and principal travel separately in `stream_context` and
                // are installed only around its synchronous query execution.
                let in_tx = runtime.connection_in_transaction(0);
                tokio::spawn(async move {
                    os::run_output_stream(
                        runtime_ref,
                        frame_id,
                        sid,
                        req,
                        in_tx,
                        cancel_rx,
                        send,
                        stream_context,
                        stream_role,
                    )
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
                        queue_send(&out_tx, encode_frame(&err)).await?;
                        continue;
                    }
                };
                // Server max-wait cap (issue #919, AC #3). Reject an
                // over-cap budget with an explicit error *before*
                // spawning the wait task — no waiter is registered and
                // the budget is never silently shortened.
                if let Err(msg) = runtime.redwire_queue_wait_cap_check(req.wait_ms) {
                    let err = qw::build_queue_wait_error_frame(
                        frame_id,
                        sid,
                        qw::WAIT_EXCEEDS_CAP_CODE,
                        &msg,
                    )
                    .map_err(|e| io::Error::other(format!("build queue-wait cap error: {e}")))?;
                    queue_send(&out_tx, encode_frame(&err)).await?;
                    continue;
                }
                let runtime_ref = Arc::clone(&runtime);
                let out = out_tx.clone();
                let queue_name = req.queue.clone();
                let wait_ms = req.wait_ms;
                let auth_identity = Some((session.username.clone(), session.role));
                let tenant = session.tenant.clone();
                // Owned by the connection-scoped `JoinSet` so a client
                // disconnect (frame loop return) aborts a still-parked
                // wait and releases its registry slot promptly (#920).
                queue_wait_tasks.spawn(async move {
                    use crate::runtime::RedwireWaitOutcome;
                    match runtime_ref
                        .redwire_queue_wait_json(
                            &req.queue,
                            req.group.as_deref(),
                            &req.consumer,
                            req.count,
                            req.wait_ms,
                            auth_identity,
                            tenant,
                        )
                        .await
                    {
                        // Happy path: push each delivered message.
                        Ok(RedwireWaitOutcome::Delivered(messages)) => {
                            for message in messages {
                                match qw::build_event_push_frame(frame_id, sid, &message) {
                                    Ok(push) => {
                                        if queue_send(&out, encode_frame(&push)).await.is_err() {
                                            return;
                                        }
                                    }
                                    Err(_) => return,
                                }
                            }
                        }
                        // Deadline elapsed with nothing deliverable: a
                        // distinct timeout frame, not an empty push and
                        // not an error (AC #1 / AC #2).
                        Ok(RedwireWaitOutcome::TimedOut) => {
                            if let Ok(t) = qw::build_queue_wait_timeout_frame(
                                frame_id,
                                sid,
                                &queue_name,
                                wait_ms,
                            ) {
                                let _ = queue_send(&out, encode_frame(&t)).await;
                            }
                        }
                        // Server-side cancellation: a StreamError with
                        // the distinct cancellation code so the client
                        // never confuses it with a timeout (AC #2).
                        Ok(RedwireWaitOutcome::Cancelled) => {
                            if let Ok(ef) = qw::build_queue_wait_error_frame(
                                frame_id,
                                sid,
                                qw::WAIT_CANCELLED_CODE,
                                "queue wait cancelled by server",
                            ) {
                                let _ = queue_send(&out, encode_frame(&ef)).await;
                            }
                        }
                        // A genuine runtime failure. Server-shutdown
                        // cancellation is surfaced as the distinct
                        // `RedwireWaitOutcome::Cancelled` arm above, so an
                        // `Err` here is never a cancellation (#920 AC #2).
                        Err(err) => {
                            if let Ok(ef) = qw::build_queue_wait_error_frame(
                                frame_id,
                                sid,
                                qw::WAIT_FAILED_CODE,
                                &err.to_string(),
                            ) {
                                let _ = queue_send(&out, encode_frame(&ef)).await;
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
                    queue_send(&out_tx, encode_frame(&err)).await?;
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
                        queue_send(&out_tx, encode_frame(&err)).await?;
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
                        execute_with_redwire_context(&operation_context, session.role, || {
                            state.commit_chunk(&runtime, &chunk.rows)
                        })
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
                        queue_send(&out_tx, encode_frame(&err)).await?;
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
                            queue_send(&out_tx, encode_frame(&end)).await?;
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
                    queue_send(&out_tx, encode_frame(&end)).await?;
                } else {
                    // AC #6: protocol violation surfaces as a
                    // StreamError envelope, not a connection drop.
                    let err = os::build_stream_error_frame(
                        frame.correlation_id,
                        sid,
                        "unknown_stream",
                        "no active stream for this stream_id",
                    )?;
                    queue_send(&out_tx, encode_frame(&err)).await?;
                }
            }
            other => {
                let err_frame = build_error_frame_lossy(
                    frame.correlation_id,
                    &format!("redwire: cannot dispatch {other:?} yet"),
                );
                queue_send(&out_tx, encode_frame(&err_frame)).await?;
            }
        }
    }
}

/// Frames a connection may have queued for its socket writer before the
/// producers block. Bounded so a peer that stops reading cannot make the
/// server buffer an output stream without limit; the writer task drains it
/// as fast as the socket accepts bytes.
const OUTBOUND_QUEUE_FRAMES: usize = 256;

async fn queue_send(out_tx: &mpsc::Sender<Vec<u8>>, bytes: Vec<u8>) -> io::Result<()> {
    out_tx
        .send(bytes)
        .await
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
    if validate_minor_version(minor).is_err() {
        // Future client speaking a version we don't know — refuse
        // immediately. We do not send a frame because the client
        // hasn't agreed on the framing version yet.
        return Ok(None);
    }

    // Step 2: read the Hello frame.
    let hello = read_frame(stream).await?;
    if hello.kind != MessageKind::Hello {
        let fail = encode_frame(&reply_frame_or_io_error(
            hello.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail_payload("first frame after magic must be Hello"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    }
    let hello_msg = match Hello::from_payload(&hello.payload) {
        Ok(h) => h,
        Err(e) => {
            let fail = encode_frame(&reply_frame_or_io_error(
                hello.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail_payload(&e),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    let Some(chosen_version) = choose_hello_minor_version(&hello_msg.versions) else {
        let fail = encode_frame(&reply_frame_or_io_error(
            hello.correlation_id,
            MessageKind::AuthFail,
            build_auth_fail_payload("no overlapping protocol version"),
        )?);
        let _ = stream.write_all(&fail).await;
        return Ok(None);
    };

    let server_anon_ok = auth_store.map(|s| !s.is_enabled()).unwrap_or(true);
    let chosen = match pick_auth_method(&hello_msg.auth_methods, server_anon_ok) {
        Some(m) => m,
        None => {
            let fail = encode_frame(&reply_frame_or_io_error(
                hello.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail_payload("no overlapping auth method"),
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
    let ack_frame = build_hello_ack_frame(
        hello.correlation_id,
        chosen_version,
        chosen,
        server_features,
        topology.as_ref(),
    )
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
    let auth_payload = match expect_auth_response_payload(resp.kind, &resp.payload, "AuthResponse")
    {
        Ok(payload) => payload,
        Err(err) => {
            let fail = encode_frame(&reply_frame_or_io_error(
                resp.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail_payload(&err.to_string()),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    // OAuth-JWT branch. The `jwt` field carries either a browser access
    // JWT (the hybrid-token model, issue #936) or a federated IdP token
    // validated by the configured `OAuthValidator`. The browser access
    // token is tried *first* and independently of `oauth` being wired, so
    // a deployment that runs the browser credential layer without any
    // external OAuth IdP still authenticates. mTLS stays native-only
    // (ADR 0036) — the browser presents this access JWT and nothing else.
    if chosen == "oauth-jwt" {
        let raw = match parse_auth_response_oauth_jwt(auth_payload) {
            Ok(raw) if !raw.is_empty() => raw,
            _ => {
                let fail = encode_frame(&reply_frame_or_io_error(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail_payload("oauth-jwt: AuthResponse missing 'jwt' string"),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };

        // 1. Browser hybrid-token access JWT (issue #936). A *valid*
        //    access token (correct issuer/audience/signature, `typ:
        //    access`, unexpired) authenticates the session directly.
        //    Anything else (expired, wrong type, or simply not one of our
        //    tokens — e.g. a foreign IdP RS256 token) falls through to the
        //    OAuth validator below; the net effect is "valid browser
        //    token accepted, expired/invalid rejected" (AC #2). The stream
        //    lease (ADR 0029) then decouples this token's expiry from any
        //    stream the session opens, so a later refresh never tears down
        //    in-flight work (AC #3).
        if let Some(authority) = runtime.browser_token_authority() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Ok(identity) = authority.validate_access(&raw, now) {
                let session_id = super::auth::new_session_id_for_scram();
                let ok = encode_frame(&reply_frame_or_io_error(
                    resp.correlation_id,
                    MessageKind::AuthOk,
                    build_auth_ok(
                        &session_id,
                        &identity.username,
                        identity.role,
                        server_features,
                    ),
                )?);
                stream.write_all(&ok).await?;
                return Ok(Some(AuthedSession {
                    username: identity.username,
                    role: identity.role,
                    tenant: identity.tenant,
                    session_id,
                }));
            }
        }

        // 2. Federated OAuth-JWT (RS256/ES256 against a configured IdP).
        let validator = match oauth {
            Some(v) => v,
            None => {
                let fail = encode_frame(&reply_frame_or_io_error(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail_payload(
                        "oauth-jwt: token rejected (no browser-token authority or OAuth validator accepted it)",
                    ),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        };
        match super::auth::validate_oauth_jwt_full(validator, &raw) {
            Ok((tenant, username, role)) => {
                let session_id = super::auth::new_session_id_for_scram();
                let ok = encode_frame(&reply_frame_or_io_error(
                    resp.correlation_id,
                    MessageKind::AuthOk,
                    build_auth_ok(&session_id, &username, role, server_features),
                )?);
                stream.write_all(&ok).await?;
                return Ok(Some(AuthedSession {
                    username,
                    role,
                    tenant,
                    session_id,
                }));
            }
            Err(reason) => {
                let fail = encode_frame(&reply_frame_or_io_error(
                    resp.correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail_payload(&format!("oauth-jwt: {reason}")),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
        }
    }

    match validate_auth_response(chosen, auth_payload, auth_store) {
        AuthOutcome::Authenticated {
            username,
            role,
            tenant,
            session_id,
        } => {
            let ok_frame = build_auth_ok_frame_from_payload(
                resp.correlation_id,
                build_auth_ok(&session_id, &username, role, server_features),
            )
            .map_err(|e| io::Error::other(format!("build AuthOk: {e}")))?;
            let ok = encode_frame(&ok_frame);
            stream.write_all(&ok).await?;
            Ok(Some(AuthedSession {
                username,
                role,
                tenant,
                session_id,
            }))
        }
        AuthOutcome::Refused(reason) => {
            let fail = encode_frame(&reply_frame_or_io_error(
                resp.correlation_id,
                MessageKind::AuthFail,
                build_auth_fail_payload(&reason),
            )?);
            let _ = stream.write_all(&fail).await;
            Ok(None)
        }
    }
}

/// 3-RTT SCRAM-SHA-256 server handshake (RFC 5802 + RFC 7677).
///
/// ```text
/// C → S  AuthResponse(client-first-message)         (already received as client-first)
/// S → C  AuthRequest(server-first-message)
/// C → S  AuthResponse(client-final-message)
/// S → C  AuthOk(v=server-signature)
/// ```
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
            let fail = encode_frame(&reply_frame_or_io_error(
                initial_correlation,
                MessageKind::AuthFail,
                build_auth_fail_payload("scram-sha-256 requires an AuthStore"),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
    };

    let mut handshake = ScramServerHandshake::new(
        super::auth::new_server_nonce(),
        crate::auth::store::random_bytes(16),
    );

    // The wire handshake doesn't yet learn
    // a tenant before the SCRAM exchange completes, so we resolve
    // against the platform tenant. Tenant-scoped users authenticate
    // through the JWT path (which carries the tenant claim) or a
    // future explicit `tenant` extension to the AuthRequest payload.
    let client_first = read_frame(stream).await?;
    let username = match handshake.step(ScramServerInput::ClientMessage {
        correlation_id: client_first.correlation_id,
        kind: client_first.kind,
        payload: client_first.payload,
    }) {
        ScramServerOutput::NeedVerifier { username } => username,
        ScramServerOutput::Failed {
            correlation_id,
            reason,
        } => {
            let fail = encode_frame(&reply_frame_or_io_error(
                correlation_id,
                MessageKind::AuthFail,
                build_auth_fail_payload(&reason),
            )?);
            let _ = stream.write_all(&fail).await;
            return Ok(None);
        }
        _ => unreachable!("client-first step must request a verifier or fail"),
    };
    let verifier = store.lookup_scram_verifier_global(&username);
    let (challenge_correlation_id, challenge_payload) =
        match handshake.step(ScramServerInput::Verifier(verifier)) {
            ScramServerOutput::Challenge {
                correlation_id,
                payload,
            } => (correlation_id, payload),
            _ => unreachable!("verifier step must produce a SCRAM challenge"),
        };
    let req = encode_frame(&reply_frame_or_io_error(
        challenge_correlation_id,
        MessageKind::AuthRequest,
        challenge_payload,
    )?);
    stream.write_all(&req).await?;

    let client_final = read_frame(stream).await?;
    let (final_correlation_id, username, server_signature) =
        match handshake.step(ScramServerInput::ClientMessage {
            correlation_id: client_final.correlation_id,
            kind: client_final.kind,
            payload: client_final.payload,
        }) {
            ScramServerOutput::Authenticated {
                correlation_id,
                username,
                server_signature,
            } => (correlation_id, username, server_signature),
            ScramServerOutput::Failed {
                correlation_id,
                reason,
            } => {
                let fail = encode_frame(&reply_frame_or_io_error(
                    correlation_id,
                    MessageKind::AuthFail,
                    build_auth_fail_payload(&reason),
                )?);
                let _ = stream.write_all(&fail).await;
                return Ok(None);
            }
            _ => unreachable!("client-final step must authenticate or fail"),
        };
    let user = store
        .list_users()
        .into_iter()
        .find(|u| u.username == username);
    let role = user
        .as_ref()
        .map(|u| u.role)
        .unwrap_or(crate::auth::Role::Read);
    let session_id = super::auth::new_session_id_for_scram();
    let ok_payload = super::auth::build_scram_auth_ok(
        &session_id,
        &username,
        role,
        server_features,
        &server_signature,
    );
    let ok = encode_frame(&reply_frame_or_io_error(
        final_correlation_id,
        MessageKind::AuthOk,
        ok_payload,
    )?);
    stream.write_all(&ok).await?;
    Ok(Some(AuthedSession {
        username,
        role,
        tenant: user.and_then(|u| u.tenant_id),
        session_id,
    }))
}

async fn read_frame<S>(stream: &mut S) -> io::Result<Frame>
where
    S: AsyncRead + Unpin + Send,
{
    read_frame_async(stream).await.map_err(redwire_io_err)
}

fn redwire_io_err(err: reddb_wire::redwire::RedWireIoError) -> io::Error {
    match err {
        reddb_wire::redwire::RedWireIoError::Io(err) => err,
        reddb_wire::redwire::RedWireIoError::Frame(err) => {
            io::Error::other(format!("decode frame: {err}"))
        }
    }
}

/// `Query` (0x01) answers with the pinned summary payload — statement type
/// plus affected rows — not the full result envelope. `QueryWithParams`
/// (0x28) is the frame that carries records back; clients pick the frame by
/// the reply shape they want.
fn run_query(runtime: &RedDBRuntime, prepared: &PreparedRegistry, frame: &Frame) -> Frame {
    let sql = match std::str::from_utf8(&frame.payload) {
        Ok(s) => s,
        Err(_) => {
            return build_error_frame_lossy(
                frame.correlation_id,
                "Query payload must be UTF-8 SQL",
            );
        }
    };
    let result =
        QueryRequestExecutor::new(runtime, prepared).execute(QueryRequest::sql(sql, Vec::new()));
    crate::presentation::query_result::summary_frame(
        frame.correlation_id,
        result.as_ref().map_err(crate::api::client_facing_message),
    )
}

fn run_query_with_params(
    runtime: &RedDBRuntime,
    prepared: &PreparedRegistry,
    frame: &Frame,
) -> Frame {
    let request = match decode_query_with_params_request(&frame.payload) {
        Ok(decoded) => decoded,
        Err(err) => return build_error_frame_lossy(frame.correlation_id, &err.to_string()),
    };
    let commit_policy = match parse_redwire_commit_policy(request.options.commit_policy.as_deref())
    {
        Ok(policy) => policy,
        Err(err) => return build_error_frame_lossy(frame.correlation_id, &err),
    };
    let params = request
        .params
        .into_iter()
        .map(param_to_request_value)
        .collect::<Vec<_>>();
    let mut query = QueryRequest::sql(request.sql, params);
    if let Some(policy) = commit_policy {
        query = query.with_commit_policy(policy);
    }
    let result = QueryRequestExecutor::new(runtime, prepared).execute(query);
    crate::presentation::query_result::envelope_frame(
        frame.correlation_id,
        result.as_ref().map_err(crate::api::client_facing_message),
    )
}

fn parse_redwire_commit_policy(
    value: Option<&str>,
) -> Result<Option<crate::replication::CommitPolicy>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    crate::replication::CommitPolicy::parse_strict(value)
        .map(Some)
        .ok_or_else(|| format!("invalid commit_policy value '{value}'"))
}

fn param_to_request_value(value: RedWireParamValue) -> ParamValue {
    match value {
        RedWireParamValue::Null => ParamValue::Null,
        RedWireParamValue::Bool(value) => ParamValue::Bool(value),
        RedWireParamValue::Int(value) => ParamValue::Int64(value),
        RedWireParamValue::Float(value) => ParamValue::Float64(value),
        RedWireParamValue::Text(value) => ParamValue::Text(value),
        RedWireParamValue::Bytes(value) => ParamValue::Bytes(value),
        RedWireParamValue::Vector(value) => ParamValue::Vector(value),
        RedWireParamValue::Json(value) => ParamValue::Json(value),
        RedWireParamValue::Timestamp(value) => ParamValue::Timestamp(value),
        RedWireParamValue::Uuid(value) => ParamValue::Uuid(value),
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
/// `INSERT … RETURNING rid` for one JSON row. A bare SQL INSERT result
/// carries only `affected_rows`; the engine-assigned id is exposed
/// through RETURNING, which is what `insert_result_to_json` reads to
/// fill the BulkOk `id`/`ids` keys.
fn insert_sql_returning_rid<'a, I>(collection: &str, fields: I) -> String
where
    I: Iterator<Item = (&'a String, &'a crate::json::Value)>,
{
    let mut sql = crate::rpc_stdio::build_insert_sql(collection, fields);
    sql.push_str(" RETURNING rid");
    sql
}

fn run_insert_dispatch(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let request = match decode_insert_dispatch_payload(&frame.payload) {
        Ok(request) => request,
        Err(err) => return build_error_frame_lossy(frame.correlation_id, &err.to_string()),
    };
    let collection = request.collection.as_str();
    if let Err(msg) = crate::rpc_stdio::ensure_sql_collection(collection) {
        return build_error_frame_lossy(frame.correlation_id, &msg);
    }
    let payload = request.payload.map(wire_json_to_server_json);
    let payloads = request.payloads.map(|rows| {
        rows.into_iter()
            .map(wire_json_to_server_json)
            .collect::<Vec<_>>()
    });

    // Analytics batch-insert path (issue #587). Either field flips the
    // dispatch — the brief carries `idempotency_key` as the canonical
    // signal; the optional `batch: true` boolean exists for callers
    // that want the validation contract without committing to a
    // replay window.
    let idempotency_key = request.idempotency_key.as_deref();
    if idempotency_key.is_some() || request.batch {
        let items = match payloads.as_deref() {
            Some(rows) => rows,
            None => {
                return build_error_frame_lossy(
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
        return build_dispatch_reply_frame(frame.correlation_id, kind, outcome.body);
    }

    if let Some(rows) = payloads.as_ref() {
        let mut objects = Vec::with_capacity(rows.len());
        for entry in rows {
            objects.push(match entry.as_object() {
                Some(o) => o,
                None => {
                    return build_error_frame_lossy(
                        frame.correlation_id,
                        "Insert: each payload must be a JSON object",
                    )
                }
            });
        }

        if crate::rpc_stdio::should_bulk_insert_graph(runtime, collection, &objects) {
            return match crate::rpc_stdio::bulk_insert_graph(runtime, collection, &objects) {
                Ok(body) => {
                    let payload = body.to_string_compact().into_bytes();
                    build_dispatch_reply_frame(frame.correlation_id, MessageKind::BulkOk, payload)
                }
                Err(err) => build_error_frame_lossy(frame.correlation_id, &err.to_string()),
            };
        }

        let mut affected: u64 = 0;
        let mut ids = Vec::with_capacity(objects.len());
        for row in objects {
            let sql = insert_sql_returning_rid(collection, row.iter());
            match runtime.execute_query(&sql) {
                Ok(qr) => {
                    affected += qr.affected_rows;
                    if let Some(id) = crate::rpc_stdio::insert_result_to_json(&qr).get("id") {
                        ids.push(id.clone());
                    }
                }
                Err(err) => {
                    return build_error_frame_lossy(
                        frame.correlation_id,
                        &crate::api::client_facing_message(&err),
                    )
                }
            }
        }
        let payload = encode_bulk_ok_payload_from_json_id_literals(
            affected,
            ids.iter().map(|id| id.to_string()),
        );
        return build_dispatch_reply_frame(frame.correlation_id, MessageKind::BulkOk, payload);
    }

    let row = match payload.as_ref().and_then(|x| x.as_object()) {
        Some(o) => o,
        None => {
            return build_error_frame_lossy(
                frame.correlation_id,
                "Insert: missing 'payload' object or 'payloads' array",
            )
        }
    };
    let sql = insert_sql_returning_rid(collection, row.iter());
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let body = crate::rpc_stdio::insert_result_to_json(&qr);
            let payload = body.to_string_compact().into_bytes();
            build_dispatch_reply_frame(frame.correlation_id, MessageKind::BulkOk, payload)
        }
        Err(err) => build_error_frame_lossy(
            frame.correlation_id,
            &crate::api::client_facing_message(&err),
        ),
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

fn reply_frame_or_io_error(
    correlation_id: u64,
    kind: MessageKind,
    payload: Vec<u8>,
) -> io::Result<Frame> {
    build_reply_frame(correlation_id, kind, payload)
        .map_err(|e| io::Error::other(format!("build {kind:?}: {e}")))
}

fn wire_json_to_server_json(value: impl std::fmt::Display) -> JsonValue {
    serde_json::from_str::<JsonValue>(&value.to_string()).unwrap_or(JsonValue::Null)
}

/// Get payload shape: `{ "collection": "...", "id": "..." }`.
/// Bridges to `SELECT * FROM <coll> WHERE rid = <id> LIMIT 1`.
/// Reply: Result frame with the row, or empty `{}` when not found.
fn run_get(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let request = match decode_get_payload(&frame.payload) {
        Ok(request) => request,
        Err(err) => return build_error_frame_lossy(frame.correlation_id, &err.to_string()),
    };
    let collection = request.collection.as_str();
    if let Err(msg) = crate::rpc_stdio::ensure_sql_collection(collection) {
        return build_error_frame_lossy(frame.correlation_id, &msg);
    }
    let Some(rid) = crate::rpc_stdio::rid_from_str(&request.id) else {
        let payload = encode_get_result_payload(false);
        return build_dispatch_reply_frame(frame.correlation_id, MessageKind::Result, payload);
    };
    let sql = format!("SELECT * FROM {collection} WHERE rid = {rid} LIMIT 1");
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            // Preserve the existing Get envelope: presence only.
            let payload = encode_get_result_payload(!qr.result.records.is_empty());
            build_dispatch_reply_frame(frame.correlation_id, MessageKind::Result, payload)
        }
        Err(err) => build_error_frame_lossy(
            frame.correlation_id,
            &crate::api::client_facing_message(&err),
        ),
    }
}

/// Delete payload shape: `{ "collection": "...", "id": "..." }`.
/// Bridges to `DELETE FROM <coll> WHERE rid = <id>`.
/// Reply: DeleteOk frame with `{ affected }`.
fn run_delete(runtime: &RedDBRuntime, frame: &Frame) -> Frame {
    let request = match decode_delete_payload(&frame.payload) {
        Ok(request) => request,
        Err(err) => return build_error_frame_lossy(frame.correlation_id, &err.to_string()),
    };
    let collection = request.collection.as_str();
    if let Err(msg) = crate::rpc_stdio::ensure_sql_collection(collection) {
        return build_error_frame_lossy(frame.correlation_id, &msg);
    }
    let Some(rid) = crate::rpc_stdio::rid_from_str(&request.id) else {
        let payload = encode_delete_ok_payload(0);
        return build_dispatch_reply_frame(frame.correlation_id, MessageKind::DeleteOk, payload);
    };
    let sql = format!("DELETE FROM {collection} WHERE rid = {rid}");
    match runtime.execute_query(&sql) {
        Ok(qr) => {
            let payload = encode_delete_ok_payload(qr.affected_rows);
            build_dispatch_reply_frame(frame.correlation_id, MessageKind::DeleteOk, payload)
        }
        Err(err) => build_error_frame_lossy(
            frame.correlation_id,
            &crate::api::client_facing_message(&err),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::RedDBRuntime;
    use std::cell::Cell;
    use std::sync::{Mutex, OnceLock};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn all_message_kinds() -> Vec<MessageKind> {
        (u8::MIN..=u8::MAX)
            .filter_map(MessageKind::from_u8)
            .collect()
    }

    #[test]
    fn every_redwire_frame_kind_resolves_to_a_catalog_command() {
        let catalog = crate::server::discovered_route_catalog();

        for kind in all_message_kinds() {
            let Some(command_id) = redwire_command_id(kind) else {
                continue;
            };
            assert!(
                catalog.command(command_id).is_some(),
                "{kind:?} maps to missing catalog command {command_id}"
            );
        }
    }

    #[test]
    fn command_coverage_matrix_reports_redwire_bindings() {
        let matrix = crate::server::route_catalog::render_command_coverage_matrix(
            crate::server::discovered_route_catalog(),
        );

        assert!(
            matrix.contains("| command | auth requirement | HTTP | gRPC | MCP | stdio | RedWire |")
        );
        // RedWire binds Query/Prepare/ExecutePrepared here; gRPC binds four rpcs
        // to it in `grpc/catalog_dispatch.rs`; MCP exposes it as `reddb_query`;
        // `rpc_stdio.rs` names no command ids at all.
        assert!(matrix.contains(
            "| query.execute | user-required | served | served | served | undeclared | served |"
        ));
        // No frame kind and no rpc carry `catalog.snapshot`; MCP reaches it
        // through the `reddb_type_of` tool.
        assert!(matrix.contains(
            "| catalog.snapshot | user-required | served | undeclared | served | undeclared | undeclared |"
        ));
    }

    struct DenyAndCountPolicy(Cell<usize>);

    impl crate::server::route_catalog::CommandPolicyEngine for DenyAndCountPolicy {
        fn allows(
            &self,
            _ctx: &OperationContext,
            _command: &crate::server::route_catalog::CommandSpec,
        ) -> bool {
            self.0.set(self.0.get() + 1);
            false
        }
    }

    #[test]
    fn redwire_dispatch_consults_catalog_authorization() {
        let policy = DenyAndCountPolicy(Cell::new(0));
        let ctx = OperationContext::read_only("redwire-auth-test").with_principal("reader");

        let error = authorize_redwire_frame(&ctx, MessageKind::Query, &policy)
            .expect_err("denying policy must reject the frame");

        assert_eq!(policy.0.get(), 1);
        assert_eq!(
            error,
            crate::server::route_catalog::CommandAuthorizationError::Denied {
                command_id: "query.execute"
            }
        );
    }

    #[test]
    fn redwire_query_uses_operation_context_tenant_for_rls() {
        crate::runtime::execution_context::clear_current_tenant();
        crate::runtime::execution_context::clear_current_auth_identity();
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let prepared = PreparedRegistry::new();
        runtime
            .execute_query("CREATE TABLE docs (id INT, tenant_id TEXT)")
            .expect("create table");
        runtime
            .execute_query(
                "INSERT INTO docs (id, tenant_id) VALUES \
                 (1, 'acme'), (2, 'acme'), (3, 'globex')",
            )
            .expect("seed rows");
        runtime
            .execute_query(
                "CREATE POLICY tenant_read ON docs FOR SELECT \
                 USING (tenant_id = CURRENT_TENANT())",
            )
            .expect("create policy");
        runtime
            .execute_query("ALTER TABLE docs ENABLE ROW LEVEL SECURITY")
            .expect("enable rls");

        let payload =
            reddb_wire::query_with_params::encode_query_with_params("SELECT id FROM docs", &[])
                .expect("encode query");
        let frame = reddb_wire::redwire::build_query_with_params_frame(7, payload)
            .expect("build query frame");

        let visible_rows = |tenant: &str| {
            let ctx = OperationContextFactory::build(OperationContextInput {
                request_id: Some(format!("redwire-rls-{tenant}")),
                principal: Some("reader".to_string()),
                tenant: Some(tenant.to_string()),
                ..OperationContextInput::default()
            });
            let reply = execute_with_redwire_context(&ctx, Role::Read, || {
                run_query_with_params(&runtime, &prepared, &frame)
            });
            assert_eq!(reply.kind, MessageKind::Result);
            let body: JsonValue = serde_json::from_slice(&reply.payload).expect("result envelope");
            body.get("result")
                .and_then(|result| result.get("records"))
                .and_then(JsonValue::as_array)
                .map_or(0, |records| records.len())
        };

        assert_eq!(visible_rows("acme"), 2);
        assert_eq!(visible_rows("globex"), 1);
        assert!(crate::runtime::execution_context::current_tenant().is_none());
        assert!(crate::runtime::execution_context::current_auth_identity().is_none());
    }

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let previous = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.iter().rev() {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn temp_data_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb-redwire-{name}-{}-{}.rdb",
            std::process::id(),
            crate::utils::now_unix_millis()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn bulk_insert_frame(correlation_id: u64, payload: Vec<u8>) -> Frame {
        reddb_wire::redwire::build_bulk_insert_frame(correlation_id, payload)
            .expect("build bulk insert frame")
    }

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

            ai_policy: None,
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

        let nodes = bulk_insert_frame(
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
        let edges = bulk_insert_frame(
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

    #[test]
    fn bulk_insert_dispatch_surfaces_row_ids() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");

        let single = bulk_insert_frame(
            9,
            br#"{"collection":"rows_rid","payload":{"name":"eve"}}"#.to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &single);
        assert_eq!(
            reply.kind,
            MessageKind::BulkOk,
            "body={:?}",
            String::from_utf8_lossy(&reply.payload)
        );
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("single json");
        assert_eq!(body.get("affected").and_then(JsonValue::as_u64), Some(1));
        assert!(
            body.get("id").and_then(JsonValue::as_u64).is_some(),
            "single insert must carry the assigned id: {body}"
        );

        let batch = bulk_insert_frame(
            10,
            br#"{"collection":"rows_rid","payloads":[{"name":"a"},{"name":"b"}]}"#.to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &batch);
        assert_eq!(reply.kind, MessageKind::BulkOk);
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("batch json");
        assert_eq!(body.get("affected").and_then(JsonValue::as_u64), Some(2));
        let ids: Vec<u64> = body
            .get("ids")
            .and_then(JsonValue::as_array)
            .expect("ids array")
            .iter()
            .map(|id| id.as_u64().expect("numeric id"))
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "ids must be distinct");
    }

    #[test]
    fn get_and_delete_dispatch_address_rows_by_rid() {
        use reddb_wire::redwire::operations::{
            decode_delete_ok_affected, decode_get_result_payload, encode_key_payload,
        };
        use reddb_wire::redwire::{build_delete_frame, build_get_frame};

        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let insert = bulk_insert_frame(
            11,
            br#"{"collection":"rows_rid_del","payload":{"name":"k"}}"#.to_vec(),
        );
        let reply = run_insert_dispatch(&runtime, &insert);
        assert_eq!(reply.kind, MessageKind::BulkOk);
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("insert json");
        let rid = body
            .get("id")
            .and_then(JsonValue::as_u64)
            .expect("assigned id");
        let rid = rid.to_string();

        let get = build_get_frame(12, encode_key_payload("rows_rid_del", &rid)).expect("get");
        let reply = run_get(&runtime, &get);
        assert_eq!(reply.kind, MessageKind::Result);
        let found = decode_get_result_payload(&reply.payload).expect("get payload");
        assert_eq!(found.get("found").and_then(|v| v.as_bool()), Some(true));

        // A non-numeric id names no row: not found / 0 affected, and the
        // text is never spliced into SQL.
        for id in ["1 OR 1=1", "'1'", "rid_that_does_not_exist"] {
            let get = build_get_frame(13, encode_key_payload("rows_rid_del", id)).expect("get");
            let reply = run_get(&runtime, &get);
            assert_eq!(reply.kind, MessageKind::Result, "{id:?}");
            let found = decode_get_result_payload(&reply.payload).expect("get payload");
            assert_eq!(found.get("found").and_then(|v| v.as_bool()), Some(false));

            let del = build_delete_frame(13, encode_key_payload("rows_rid_del", id)).expect("del");
            let reply = run_delete(&runtime, &del);
            assert_eq!(reply.kind, MessageKind::DeleteOk, "{id:?}");
            assert_eq!(
                decode_delete_ok_affected(&reply.payload).expect("affected"),
                0
            );
        }
        // A collection that is not a bare identifier is refused outright.
        let del = build_delete_frame(13, encode_key_payload("rows_rid_del WHERE 1=1 --", &rid))
            .expect("del");
        let reply = run_delete(&runtime, &del);
        assert_eq!(reply.kind, MessageKind::Error);

        let del = build_delete_frame(14, encode_key_payload("rows_rid_del", &rid)).expect("del");
        let reply = run_delete(&runtime, &del);
        assert_eq!(reply.kind, MessageKind::DeleteOk);
        assert_eq!(
            decode_delete_ok_affected(&reply.payload).expect("affected"),
            1
        );

        let reply = run_delete(&runtime, &del);
        assert_eq!(reply.kind, MessageKind::DeleteOk);
        assert_eq!(
            decode_delete_ok_affected(&reply.payload).expect("affected"),
            0
        );
    }

    #[test]
    fn redwire_query_with_params_preserves_json_columns() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let prepared = PreparedRegistry::new();
        runtime
            .execute_query("KV PUT proj.a.b.c.d = 12")
            .expect("put nested number");
        runtime
            .execute_query("KV PUT proj.a.b.e = 'x'")
            .expect("put nested string");

        let payload =
            reddb_wire::query_with_params::encode_query_with_params("LIST KV proj AS JSON", &[])
                .expect("encode query with params");
        let frame = reddb_wire::redwire::build_query_with_params_frame(99, payload)
            .expect("query-with-params frame");
        let reply = run_query_with_params(&runtime, &prepared, &frame);

        assert_eq!(
            reply.kind,
            MessageKind::Result,
            "body={}",
            String::from_utf8_lossy(&reply.payload)
        );
        let body: JsonValue = serde_json::from_slice(&reply.payload).expect("result json");
        let value = body
            .get("result")
            .and_then(|result| result.get("records"))
            .and_then(JsonValue::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("values"))
            .and_then(|values| values.get("value"))
            .expect("json value column");

        assert_eq!(
            value
                .get("a")
                .and_then(|a| a.get("b"))
                .and_then(|b| b.get("c"))
                .and_then(|c| c.get("d"))
                .and_then(JsonValue::as_f64),
            Some(12.0)
        );
        assert_eq!(
            value
                .get("a")
                .and_then(|a| a.get("b"))
                .and_then(|b| b.get("e"))
                .and_then(JsonValue::as_str),
            Some("x")
        );
    }

    /// The two query frames answer with deliberately different shapes:
    /// `Query` (0x01) replies with the summary payload the protocol pins,
    /// `QueryWithParams` (0x28) with the full result envelope. Clients pick
    /// the frame by the reply they want, so neither may drift into the other.
    #[test]
    fn redwire_query_replies_summary_and_query_with_params_replies_full_envelope() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let prepared = PreparedRegistry::new();

        let query =
            reddb_wire::redwire::build_query_frame(98, "SELECT 7 AS value").expect("query frame");
        let query_reply = run_query(&runtime, &prepared, &query);
        assert_eq!(query_reply.kind, MessageKind::Result);
        let summary =
            reddb_wire::redwire::operations::decode_query_result_payload(&query_reply.payload)
                .expect("Query replies with the summary payload");
        assert_eq!(
            summary.get("statement").and_then(|value| value.as_str()),
            Some("select")
        );
        assert_eq!(
            summary.get("affected").and_then(|value| value.as_u64()),
            Some(0),
            "affected is pinned even when it is 0, got {summary}"
        );
        assert!(
            summary.get("result").is_none(),
            "Query must not carry the full result envelope, got {summary}"
        );

        let payload = reddb_wire::query_with_params::encode_query_with_params(
            "SELECT $1 AS value",
            &[reddb_wire::query_with_params::ParamValue::Int(7)],
        )
        .expect("encode query with params");
        let query_with_params = reddb_wire::redwire::build_query_with_params_frame(99, payload)
            .expect("query-with-params frame");
        let params_reply = run_query_with_params(&runtime, &prepared, &query_with_params);
        assert_eq!(params_reply.kind, MessageKind::Result);
        let body: JsonValue =
            serde_json::from_slice(&params_reply.payload).expect("full result envelope");
        assert_eq!(
            body.get("result")
                .and_then(|result| result.get("records"))
                .and_then(JsonValue::as_array)
                .and_then(|records| records.first())
                .and_then(|record| record.get("values"))
                .and_then(|values| values.get("value"))
                .and_then(JsonValue::as_f64),
            Some(7.0)
        );
    }

    /// Issue #2139 AC1 — the prepared frames run on the connection's shared
    /// registry, so a prepared id is scoped to the connection that minted it,
    /// DDL invalidates the shape, and the kill switch closes both frames.
    /// Driven through the same handler + rewrap the session loop uses, so the
    /// legacy reply encodings are exercised too.
    #[test]
    fn redwire_prepared_requests_use_connection_registry_guards() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE redwire_prepared (id INTEGER)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO redwire_prepared (id) VALUES (7)")
            .expect("seed row");
        let mut first_connection = crate::wire::listener::PreparedStatements::default();
        let second_connection = crate::wire::listener::PreparedStatements::default();

        let prepare_payload = reddb_wire::redwire::encode_prepare_payload(
            41,
            "SELECT * FROM redwire_prepared WHERE id = 7",
        )
        .expect("encode prepare");
        let prepared = rewrap_length_prefixed_handler_response(
            &crate::wire::listener::handle_prepare(
                &runtime,
                &prepare_payload,
                &mut first_connection,
            ),
            100,
        );
        assert_eq!(prepared.kind, MessageKind::PreparedOk);
        let parameter_count = u16::from_le_bytes([prepared.payload[4], prepared.payload[5]]);
        assert_eq!(
            parameter_count, 1,
            "the literal `7` is auto-parameterized into one bind"
        );

        let execute_payload = reddb_wire::redwire::encode_execute_prepared_payload(
            41,
            &[reddb_wire::legacy::WireValue::I64(7)],
        )
        .expect("encode execute prepared");
        let executed = rewrap_length_prefixed_handler_response(
            &crate::wire::listener::handle_execute_prepared(
                &runtime,
                &execute_payload,
                &first_connection,
            ),
            101,
        );
        assert_eq!(executed.kind, MessageKind::Result);

        let wrong_connection = rewrap_length_prefixed_handler_response(
            &crate::wire::listener::handle_execute_prepared(
                &runtime,
                &execute_payload,
                &second_connection,
            ),
            102,
        );
        assert_eq!(wrong_connection.kind, MessageKind::Error);
        assert!(
            String::from_utf8_lossy(&wrong_connection.payload).contains("unknown prepared stmt_id")
        );

        runtime
            .execute_query("CREATE TABLE redwire_prepared_epoch (id INTEGER)")
            .expect("advance DDL epoch");
        let stale = rewrap_length_prefixed_handler_response(
            &crate::wire::listener::handle_execute_prepared(
                &runtime,
                &execute_payload,
                &first_connection,
            ),
            103,
        );
        assert_eq!(stale.kind, MessageKind::Error);
        assert_eq!(
            String::from_utf8_lossy(&stale.payload),
            "prepared_needs_replan"
        );

        first_connection.registry().disable();
        let disabled = rewrap_length_prefixed_handler_response(
            &crate::wire::listener::handle_execute_prepared(
                &runtime,
                &execute_payload,
                &first_connection,
            ),
            104,
        );
        assert_eq!(disabled.kind, MessageKind::Error);
        assert_eq!(
            String::from_utf8_lossy(&disabled.payload),
            "prepared statements disabled"
        );
    }

    #[test]
    fn redwire_query_with_params_request_policy_strengthens_to_ack_n() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _env = EnvGuard::set(&[
            ("RED_PRIMARY_COMMIT_POLICY", "local"),
            ("RED_REPLICATION_ACK_TIMEOUT_MS", "20"),
            ("RED_COMMIT_FAIL_ON_TIMEOUT", "true"),
        ]);
        let data_path = temp_data_path("request-ack-n");
        let runtime = RedDBRuntime::with_options(
            crate::api::RedDBOptions::persistent(&data_path)
                .with_replication(crate::replication::ReplicationConfig::primary()),
        )
        .expect("runtime");
        let prepared = PreparedRegistry::new();

        let payload = reddb_wire::query_with_params::encode_query_with_params_request(
            "INSERT INTO redwire_request_ack (id, name) VALUES (1, 'alpha')",
            &[],
            &reddb_wire::query_with_params::QueryWithParamsOptions {
                commit_policy: Some("ack_n=1".to_string()),
            },
        )
        .expect("encode query with request policy");
        let frame = reddb_wire::redwire::build_query_with_params_frame(100, payload)
            .expect("query-with-params frame");
        let reply = run_query_with_params(&runtime, &prepared, &frame);

        assert_eq!(reply.kind, MessageKind::Error);
        let body = String::from_utf8_lossy(&reply.payload);
        assert!(
            body.contains("commit policy timed out") && body.contains("RED_COMMIT_FAIL_ON_TIMEOUT"),
            "request ack_n should wait for replica ack, got {body}"
        );
        let _ = std::fs::remove_file(data_path);
    }

    #[test]
    fn redwire_query_with_params_rejects_request_policy_below_floor() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _env = EnvGuard::set(&[("RED_PRIMARY_COMMIT_POLICY", "quorum")]);
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let prepared = PreparedRegistry::new();
        runtime
            .execute_query("CREATE TABLE redwire_request_floor (id INTEGER, name TEXT)")
            .expect("create table");

        let payload = reddb_wire::query_with_params::encode_query_with_params_request(
            "INSERT INTO redwire_request_floor (id, name) VALUES (1, 'alpha')",
            &[],
            &reddb_wire::query_with_params::QueryWithParamsOptions {
                commit_policy: Some("local".to_string()),
            },
        )
        .expect("encode query with request policy");
        let frame = reddb_wire::redwire::build_query_with_params_frame(101, payload)
            .expect("query-with-params frame");
        let reply = run_query_with_params(&runtime, &prepared, &frame);

        assert_eq!(reply.kind, MessageKind::Error);
        let body = String::from_utf8_lossy(&reply.payload);
        assert!(
            body.contains("COMMIT_POLICY_BELOW_FLOOR"),
            "typed floor violation should be surfaced, got {body}"
        );

        // Ordering is observable, so pin it: the Request seam rejects a
        // below-floor policy *before* the write lands. RedWire used to run
        // the INSERT and then fail the policy check, leaving the row behind.
        let rows = runtime
            .execute_query("SELECT id FROM redwire_request_floor")
            .expect("read back the rejected write");
        assert!(
            rows.result.records.is_empty(),
            "a policy-rejected write must not be visible"
        );
    }

    /// The pre-write policy check is for mutations only. A read carrying a
    /// below-floor policy still returns rows, exactly as it did before the
    /// Request seam existed.
    #[test]
    fn redwire_query_with_params_read_ignores_request_policy_below_floor() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _env = EnvGuard::set(&[("RED_PRIMARY_COMMIT_POLICY", "quorum")]);
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let prepared = PreparedRegistry::new();
        runtime
            .execute_query("CREATE TABLE redwire_request_read (id INTEGER)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO redwire_request_read (id) VALUES (1)")
            .expect("seed row");

        let payload = reddb_wire::query_with_params::encode_query_with_params_request(
            "SELECT id FROM redwire_request_read",
            &[],
            &reddb_wire::query_with_params::QueryWithParamsOptions {
                commit_policy: Some("local".to_string()),
            },
        )
        .expect("encode query with request policy");
        let frame = reddb_wire::redwire::build_query_with_params_frame(102, payload)
            .expect("query-with-params frame");
        let reply = run_query_with_params(&runtime, &prepared, &frame);

        assert_eq!(
            reply.kind,
            MessageKind::Result,
            "body={}",
            String::from_utf8_lossy(&reply.payload)
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

        let frame = bulk_insert_frame(
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
                Some(reddb_types::Value::Text(s)) => Some(s.to_string()),
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
        let frame = bulk_insert_frame(
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

        let frame1 = bulk_insert_frame(
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
        let frame2 = bulk_insert_frame(
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

        let frame = bulk_insert_frame(
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

        let frame = bulk_insert_frame(
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
        let frame = bulk_insert_frame(500, frame_body.into_bytes());
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
}

#[cfg(test)]
mod session_bulk_stream_tests;
