//! Issue #917 / PRD #915 — RedWire live queue-wait end-to-end happy
//! path.
//!
//! A RedWire client opens a wait on an empty queue, then a producer
//! makes a message deliverable *after* the wait opened. The awaiting
//! session — parked on the queue-wait registry's async wake head, with
//! no blocking OS thread — re-probes the normal delivery path on wake
//! and pushes a `QueueEventPush` carrying the delivered message.
//!
//! The delivery test runs on a **current-thread** tokio runtime on
//! purpose: the producer (the test task) and the awaiting session task
//! share one OS thread, so if the wait held that thread the push could
//! never run and the test would deadlock against its own timeout. A
//! clean pass is therefore also the AC that the session does not hold a
//! blocking OS thread for the wait duration.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use reddb_server::auth::enforcement_mode::PolicyEnforcementMode;
use reddb_server::auth::store::PrincipalRef;
use reddb_server::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb_server::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
};
use reddb_server::server::RedDBServer;
use reddb_server::storage::query::unified::UnifiedRecord;
use reddb_server::storage::schema::Value;
use reddb_server::wire::redwire::{
    decode_frame, encode_frame, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};
use reddb_server::{RedDBOptions, RedDBRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(20);

struct ServerHandle {
    addr: SocketAddr,
    runtime: Arc<RedDBRuntime>,
    auth_store: Option<Arc<AuthStore>>,
    _join: tokio::task::JoinHandle<()>,
}

async fn start_server() -> ServerHandle {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let rt = runtime.clone();
    let join = tokio::spawn(async move {
        let _ = reddb_server::wire::redwire::start_redwire_listener_on(listener, rt).await;
    });
    ServerHandle {
        addr,
        runtime,
        auth_store: None,
        _join: join,
    }
}

async fn start_auth_server() -> ServerHandle {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let mut auth = AuthConfig::default();
    auth.enabled = true;
    let store = Arc::new(AuthStore::new(auth));
    store.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    runtime.set_auth_store(Arc::clone(&store));
    let rt = runtime.clone();
    let join = tokio::spawn(async move {
        let _ = reddb_server::wire::redwire::start_redwire_listener_on(listener, rt).await;
    });
    ServerHandle {
        addr,
        runtime,
        auth_store: Some(store),
        _join: join,
    }
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_policy(store: &AuthStore, principal: UserId, id: &str, statements: &str) {
    let policy = format!(
        r#"{{
        "id":"{id}",
        "version":1,
        "statements":{statements}
    }}"#
    );
    store
        .put_policy(reddb_server::auth::policies::Policy::from_json_str(&policy).unwrap())
        .unwrap();
    store
        .attach_policy(PrincipalRef::User(principal), id)
        .unwrap();
}

/// Read one full RedWire frame off the socket: 16-byte header, then the
/// declared payload length, then decode.
async fn read_frame(stream: &mut TcpStream) -> Frame {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    timeout(EXCHANGE_TIMEOUT, stream.read_exact(&mut header))
        .await
        .expect("frame header within budget")
        .expect("read header");
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut buf = vec![0u8; length];
    buf[..FRAME_HEADER_SIZE].copy_from_slice(&header);
    if length > FRAME_HEADER_SIZE {
        timeout(
            EXCHANGE_TIMEOUT,
            stream.read_exact(&mut buf[FRAME_HEADER_SIZE..]),
        )
        .await
        .expect("frame payload within budget")
        .expect("read payload");
    }
    let (frame, _) = decode_frame(&buf).expect("decode frame");
    frame
}

async fn write_frame(stream: &mut TcpStream, frame: &Frame) {
    stream
        .write_all(&encode_frame(frame))
        .await
        .expect("write frame");
}

fn scrape_metrics(runtime: &RedDBRuntime) -> String {
    let server = RedDBServer::new(runtime.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind metrics listener");
    let addr = listener.local_addr().expect("metrics addr");
    let handle = thread::spawn(move || server.serve_one_on(listener));

    let mut stream = StdTcpStream::connect(addr).expect("connect metrics");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set metrics read timeout");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write metrics request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read metrics response");
    handle
        .join()
        .expect("metrics server thread joined")
        .expect("metrics request served");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "metrics scrape should return 200, got {response:?}"
    );
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

fn assert_metric_line(body: &str, line: &str) {
    assert!(
        body.lines().any(|candidate| candidate == line),
        "missing metric line {line:?}. body:\n{body}"
    );
}

fn read_redwire_wait_function_source() -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set under cargo");
    let path = std::path::Path::new(&manifest).join("src/runtime/impl_queue.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    let needle = "async fn redwire_queue_wait_json";
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("could not find {needle} in impl_queue.rs"));
    let after_sig = &source[start..];
    let open = after_sig.find('{').expect("open brace after fn signature");
    let mut depth = 0i32;
    let mut end = open;
    for (i, ch) in after_sig[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    after_sig[..=end].to_string()
}

/// Drive the anonymous handshake and return the connected, authed
/// stream ready for the data-plane frame loop.
async fn connect_and_handshake(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    // Startup magic + negotiated minor version byte.
    stream.write_all(&[REDWIRE_MAGIC]).await.unwrap();
    stream.write_all(&[MAX_KNOWN_MINOR_VERSION]).await.unwrap();
    // Hello: advertise v1 + anonymous.
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::Hello,
            1,
            br#"{"versions":[1],"auth_methods":["anonymous"],"features":0,"client_name":"redwire-smoke"}"#.to_vec(),
        ),
    )
    .await;
    let ack = read_frame(&mut stream).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");
    // AuthResponse: anonymous carries no proof.
    write_frame(
        &mut stream,
        &Frame::new(MessageKind::AuthResponse, 2, b"{}".to_vec()),
    )
    .await;
    let ok = read_frame(&mut stream).await;
    assert_eq!(ok.kind, MessageKind::AuthOk, "expected AuthOk");
    stream
}

async fn connect_and_handshake_bearer(addr: SocketAddr, token: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(&[REDWIRE_MAGIC]).await.unwrap();
    stream.write_all(&[MAX_KNOWN_MINOR_VERSION]).await.unwrap();
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::Hello,
            1,
            br#"{"versions":[1],"auth_methods":["bearer"],"features":0,"client_name":"redwire-smoke"}"#.to_vec(),
        ),
    )
    .await;
    let ack = read_frame(&mut stream).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");
    let auth = serde_json::json!({ "token": token });
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::AuthResponse,
            2,
            serde_json::to_vec(&auth).unwrap(),
        ),
    )
    .await;
    let ok = read_frame(&mut stream).await;
    assert_eq!(ok.kind, MessageKind::AuthOk, "expected AuthOk");
    stream
}

fn queue_wait_open(correlation_id: u64, stream_id: u16, queue: &str, consumer: &str) -> Frame {
    let payload = serde_json::json!({
        "queue": queue,
        "group": "workers",
        "consumer": consumer,
        "count": 1,
        "wait_ms": 700
    });
    Frame::new(
        MessageKind::QueueWaitOpen,
        correlation_id,
        serde_json::to_vec(&payload).unwrap(),
    )
    .with_stream(stream_id)
}

fn text(row: &UnifiedRecord, key: &str) -> String {
    match row.get(key) {
        Some(Value::Text(s)) => s.to_string(),
        other => panic!("expected text field {key}, got {other:?}"),
    }
}

fn uint(row: &UnifiedRecord, key: &str) -> u64 {
    match row.get(key) {
        Some(Value::UnsignedInteger(u)) => *u,
        Some(Value::Integer(i)) => *i as u64,
        other => panic!("expected uint field {key}, got {other:?}"),
    }
}

fn wait_count(rows: &Vec<((String, String), u64)>, scope: &str, queue: &str) -> u64 {
    rows.iter()
        .find(|((s, q), _)| s == scope && q == queue)
        .map(|(_, n)| *n)
        .unwrap_or(0)
}

async fn wait_for_waiters(runtime: &RedDBRuntime, scope: &str, queue: &str, target: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let snap = runtime.queue_telemetry_snapshot();
        if wait_count(&snap.wait_started, scope, queue) >= target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snap = runtime.queue_telemetry_snapshot();
    panic!(
        "waiters never registered for ({scope}, {queue}); got {}",
        wait_count(&snap.wait_started, scope, queue)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wait_opened_then_post_open_push_is_delivered() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    // Open the wait on the (currently empty) queue, stream_id 3.
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        77,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":5000}"#.to_vec(),
    )
    .with_stream(3);
    write_frame(&mut client, &open).await;

    // Make a message deliverable AFTER the wait opened — it did not
    // exist at open time. The producer is this (single) thread; if the
    // session held a blocking OS thread for the wait, this push could
    // not run and the read below would hit its timeout.
    tokio::time::sleep(Duration::from_millis(150)).await;
    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'hello-live'")
        .expect("push");

    // The push frame arrives over the session writer-drain.
    let push = read_frame(&mut client).await;
    assert_eq!(
        push.kind,
        MessageKind::QueueEventPush,
        "expected a QueueEventPush"
    );
    assert_eq!(push.stream_id, 3, "push echoes the wait's stream_id");
    assert_eq!(push.correlation_id, 77, "push echoes the open correlation");

    let body = String::from_utf8(push.payload.clone()).expect("utf8 payload");
    assert!(
        body.contains("hello-live"),
        "push must carry the delivered payload, got {body}"
    );
    assert!(
        body.contains("\"message_id\""),
        "push must carry the message_id, got {body}"
    );

    let metrics = scrape_metrics(&server.runtime);
    assert_metric_line(
        &metrics,
        "queue_wait_started_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_woken_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_duration_ms_count{queue=\"jobs\",scope=\"\"} 1",
    );
}

#[test]
fn redwire_wait_outcomes_do_not_emit_audit_or_operator_events() {
    let wait_fn_source = read_redwire_wait_function_source();
    assert!(
        !wait_fn_source.contains("OperatorEvent"),
        "redwire_queue_wait_json must not emit operator events"
    );
    assert!(
        !wait_fn_source.contains("emit_global"),
        "redwire_queue_wait_json must not call operator emit_global"
    );
    assert!(
        !wait_fn_source.contains("AuditValue"),
        "redwire_queue_wait_json must not construct audit payloads"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_waiters_single_delivery_uses_normal_arbitration() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;
    for i in 0..5u16 {
        write_frame(
            &mut client,
            &queue_wait_open(100 + u64::from(i), i + 1, "jobs", &format!("c{i}")),
        )
        .await;
    }
    wait_for_waiters(&server.runtime, "", "jobs", 5).await;

    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'one'")
        .expect("push");

    let mut pushed = 0usize;
    let mut timed_out = 0usize;
    for _ in 0..5 {
        let frame = read_frame(&mut client).await;
        match frame.kind {
            MessageKind::QueueEventPush => {
                pushed += 1;
                let body = String::from_utf8(frame.payload.clone()).expect("utf8 payload");
                assert!(body.contains("one"), "delivery body was {body}");
            }
            MessageKind::QueueWaitTimeout => timed_out += 1,
            other => panic!("unexpected wait outcome frame: {other:?}"),
        }
    }

    assert_eq!(
        pushed, 1,
        "one deliverable message must produce one RedWire delivery"
    );
    assert_eq!(
        timed_out, 4,
        "losing waiters must re-park and time out after arbitration"
    );
    let snap = server.runtime.queue_telemetry_snapshot();
    assert_eq!(wait_count(&snap.wait_started, "", "jobs"), 5);
    assert_eq!(wait_count(&snap.wait_woken, "", "jobs"), 1);
    assert_eq!(wait_count(&snap.wait_timed_out, "", "jobs"), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn redwire_delivery_preserves_ack_nack_and_dlq_lifecycle() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs WITH DLQ failed_jobs MAX_ATTEMPTS 1")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    write_frame(&mut client, &queue_wait_open(200, 20, "jobs", "ack_worker")).await;
    wait_for_waiters(&server.runtime, "", "jobs", 1).await;
    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'ack-me'")
        .expect("push ack");
    let ack_delivery = read_frame(&mut client).await;
    assert_eq!(ack_delivery.kind, MessageKind::QueueEventPush);
    let ack_body: serde_json::Value = serde_json::from_slice(&ack_delivery.payload).unwrap();
    let ack_id = ack_body["message_id"].as_str().expect("message_id");
    server
        .runtime
        .execute_query(&format!("QUEUE ACK jobs GROUP workers '{ack_id}'"))
        .expect("ack");
    let empty = server
        .runtime
        .execute_query("QUEUE READ jobs GROUP workers CONSUMER verifier COUNT 1")
        .expect("read after ack");
    assert!(
        empty.result.records.is_empty(),
        "ACK must retire the RedWire-delivered message"
    );

    write_frame(
        &mut client,
        &queue_wait_open(201, 21, "jobs", "nack_worker"),
    )
    .await;
    wait_for_waiters(&server.runtime, "", "jobs", 2).await;
    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'retry-me'")
        .expect("push retry");
    let first = read_frame(&mut client).await;
    assert_eq!(first.kind, MessageKind::QueueEventPush);
    let first_body: serde_json::Value = serde_json::from_slice(&first.payload).unwrap();
    let retry_id = first_body["message_id"].as_str().expect("message_id");
    server
        .runtime
        .execute_query(&format!("QUEUE NACK jobs GROUP workers '{retry_id}'"))
        .expect("nack moves to dlq");

    let main_len = server
        .runtime
        .execute_query("QUEUE LEN jobs")
        .expect("main len");
    assert_eq!(uint(&main_len.result.records[0], "len"), 0);
    let dlq_len = server
        .runtime
        .execute_query("QUEUE LEN failed_jobs")
        .expect("dlq len");
    assert_eq!(uint(&dlq_len.result.records[0], "len"), 1);
    let dlq_peek = server
        .runtime
        .execute_query("QUEUE PEEK failed_jobs")
        .expect("dlq peek");
    assert_eq!(dlq_peek.result.records.len(), 1);
    assert_eq!(text(&dlq_peek.result.records[0], "payload"), "retry-me");
}

#[tokio::test(flavor = "current_thread")]
async fn authz_denial_and_tenant_scope_apply_to_redwire_wait() {
    let server = start_auth_server().await;
    let store = server.auth_store.as_ref().expect("auth store");
    as_user("admin", Role::Admin, || {
        server
            .runtime
            .execute_query("CREATE QUEUE jobs")
            .expect("create queue");
        server
            .runtime
            .execute_query("QUEUE GROUP CREATE jobs workers")
            .expect("create group");
    });

    let session = store.authenticate("alice", "p").expect("login");
    attach_policy(
        store,
        UserId::platform("alice"),
        "alice-peek-only",
        r#"[{"effect":"allow","actions":["queue:peek"],"resources":["queue:jobs"]}]"#,
    );
    let mut denied = connect_and_handshake_bearer(server.addr, &session.token).await;
    write_frame(&mut denied, &queue_wait_open(300, 30, "jobs", "alice")).await;
    let err = read_frame(&mut denied).await;
    assert_eq!(err.kind, MessageKind::StreamError);
    let body = String::from_utf8(err.payload.clone()).expect("utf8 payload");
    assert!(body.contains("queue_wait_failed"), "got {body}");
    assert!(body.contains("action=`queue:read`"), "got {body}");
    assert!(body.contains("denied by IAM policy"), "got {body}");

    store
        .create_user_in_tenant(Some("acme"), "tenant_alice", "p", Role::Write)
        .unwrap();
    attach_policy(
        store,
        UserId::scoped("acme", "tenant_alice"),
        "tenant-queue-read",
        r#"[{"effect":"allow","actions":["queue:read"],"resources":["queue:jobs"]}]"#,
    );
    let tenant_session = store
        .authenticate_in_tenant(Some("acme"), "tenant_alice", "p")
        .expect("tenant login");
    let mut scoped = connect_and_handshake_bearer(server.addr, &tenant_session.token).await;
    write_frame(
        &mut scoped,
        &queue_wait_open(301, 31, "jobs", "tenant_alice"),
    )
    .await;
    let timeout = read_frame(&mut scoped).await;
    assert_eq!(timeout.kind, MessageKind::QueueWaitTimeout);

    let snap = server.runtime.queue_telemetry_snapshot();
    assert_eq!(
        wait_count(&snap.wait_started, "acme", "jobs"),
        1,
        "tenant-authenticated RedWire waits must use the tenant wait scope"
    );
    assert_eq!(
        wait_count(&snap.wait_started, "", "jobs"),
        0,
        "tenant wait must not register in the platform queue scope"
    );
    clear_current_tenant();
}

/// Issue #919 / AC #1, #2, #4 — a wait that elapses with no deliverable
/// message returns a distinct `QueueWaitTimeout` frame (not a
/// `QueueEventPush`, not a `StreamError`), and the session stays
/// responsive afterwards.
///
/// Runs on a **current-thread** runtime: the test task and the awaiting
/// session task share one OS thread, so a clean pass also proves the
/// expired wait held no blocking OS thread and released its worker —
/// the Ping/Pong round-trip after the timeout would deadlock otherwise.
#[tokio::test(flavor = "current_thread")]
async fn wait_times_out_with_distinct_frame() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    // Open a 300 ms wait on the (empty) queue, stream_id 9. No producer
    // ever makes a message deliverable, so the budget elapses.
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        42,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":300}"#.to_vec(),
    )
    .with_stream(9);
    let started = std::time::Instant::now();
    write_frame(&mut client, &open).await;

    let timeout_frame = read_frame(&mut client).await;
    let elapsed = started.elapsed();
    assert_eq!(
        timeout_frame.kind,
        MessageKind::QueueWaitTimeout,
        "elapsed wait must surface a distinct QueueWaitTimeout frame, got {:?}",
        timeout_frame.kind
    );
    assert_ne!(
        timeout_frame.kind,
        MessageKind::QueueEventPush,
        "timeout must not alias a delivery"
    );
    assert_ne!(
        timeout_frame.kind,
        MessageKind::StreamError,
        "timeout must not alias an error/cancellation"
    );
    assert_eq!(
        timeout_frame.stream_id, 9,
        "timeout echoes the open stream_id"
    );
    assert_eq!(
        timeout_frame.correlation_id, 42,
        "timeout echoes the open correlation"
    );
    let body = String::from_utf8(timeout_frame.payload.clone()).expect("utf8 payload");
    assert!(
        body.contains("\"outcome\":\"timeout\""),
        "timeout body should mark the outcome, got {body}"
    );
    assert!(
        elapsed >= Duration::from_millis(250),
        "should park ~the budget before timing out, elapsed={elapsed:?}"
    );

    // The session is still reading frames on this connection — Ping →
    // Pong proves the expired wait released its worker (AC #4) and did
    // not wedge the session loop.
    write_frame(&mut client, &Frame::new(MessageKind::Ping, 43, Vec::new())).await;
    let pong = read_frame(&mut client).await;
    assert_eq!(
        pong.kind,
        MessageKind::Pong,
        "session must stay responsive after a timed-out wait"
    );

    let metrics = scrape_metrics(&server.runtime);
    assert_metric_line(
        &metrics,
        "queue_wait_started_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_timed_out_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_duration_ms_count{queue=\"jobs\",scope=\"\"} 1",
    );
}

/// Issue #920/#922 — a server-side cancellation over the live RedWire
/// queue-wait path is a normal wait outcome: distinct StreamError on
/// the wire, Prometheus counter + duration sample, no audit/operator
/// event channel involved.
#[tokio::test(flavor = "current_thread")]
async fn wait_cancelled_records_prometheus_counter() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        66,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":5000}"#.to_vec(),
    )
    .with_stream(13);
    write_frame(&mut client, &open).await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    server.runtime.queue_wait_registry().cancel_all();

    let err = read_frame(&mut client).await;
    assert_eq!(
        err.kind,
        MessageKind::StreamError,
        "cancelled wait must surface as StreamError"
    );
    assert_eq!(err.stream_id, 13, "error echoes the open stream_id");
    assert_eq!(err.correlation_id, 66, "error echoes the open correlation");
    let body = String::from_utf8(err.payload.clone()).expect("utf8 payload");
    assert!(
        body.contains("queue_wait_cancelled"),
        "cancellation must carry the cancellation code, got {body}"
    );

    let metrics = scrape_metrics(&server.runtime);
    assert_metric_line(
        &metrics,
        "queue_wait_started_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_cancelled_total{queue=\"jobs\",scope=\"\"} 1",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_duration_ms_count{queue=\"jobs\",scope=\"\"} 1",
    );

    server.runtime.queue_wait_registry().reset_cancelled();
}

/// Issue #922 — multiple live RedWire waiters share the registry's
/// wake-all path. Metrics must count every waiter that resolves through
/// delivery, not just the first waiter on the slot.
#[tokio::test(flavor = "current_thread")]
async fn wake_all_waiters_record_prometheus_counters() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut c1 = connect_and_handshake(server.addr).await;
    let mut c2 = connect_and_handshake(server.addr).await;

    write_frame(
        &mut c1,
        &Frame::new(
            MessageKind::QueueWaitOpen,
            81,
            br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":5000}"#
                .to_vec(),
        )
        .with_stream(21),
    )
    .await;
    write_frame(
        &mut c2,
        &Frame::new(
            MessageKind::QueueWaitOpen,
            82,
            br#"{"queue":"jobs","group":"workers","consumer":"c2","count":1,"wait_ms":5000}"#
                .to_vec(),
        )
        .with_stream(22),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'first-live'")
        .expect("push first");
    server
        .runtime
        .execute_query("QUEUE PUSH jobs 'second-live'")
        .expect("push second");

    let f1 = read_frame(&mut c1);
    let f2 = read_frame(&mut c2);
    let (first, second) = tokio::join!(f1, f2);
    for frame in [&first, &second] {
        assert_eq!(
            frame.kind,
            MessageKind::QueueEventPush,
            "each waiter should receive a delivery, got {:?}",
            frame.kind
        );
    }

    let metrics = scrape_metrics(&server.runtime);
    assert_metric_line(
        &metrics,
        "queue_wait_started_total{queue=\"jobs\",scope=\"\"} 2",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_woken_total{queue=\"jobs\",scope=\"\"} 2",
    );
    assert_metric_line(
        &metrics,
        "queue_wait_duration_ms_count{queue=\"jobs\",scope=\"\"} 2",
    );
}

/// Issue #919 / AC #3 — a wait whose requested budget exceeds the
/// server's maximum cap is rejected with an explicit `StreamError`
/// (carrying the cap code and naming the config key), not silently
/// shortened and never parked.
#[tokio::test(flavor = "current_thread")]
async fn wait_above_server_cap_is_rejected() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    // Default cap is 60_000 ms; ask for 1 hour. The request must be
    // refused before any parking, so the reply arrives promptly.
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        55,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":3600000}"#
            .to_vec(),
    )
    .with_stream(11);
    let started = std::time::Instant::now();
    write_frame(&mut client, &open).await;

    let err = read_frame(&mut client).await;
    let elapsed = started.elapsed();
    assert_eq!(
        err.kind,
        MessageKind::StreamError,
        "over-cap wait must be rejected with a StreamError, got {:?}",
        err.kind
    );
    assert_eq!(err.stream_id, 11, "error echoes the open stream_id");
    assert_eq!(err.correlation_id, 55, "error echoes the open correlation");
    let body = String::from_utf8(err.payload.clone()).expect("utf8 payload");
    assert!(
        body.contains("queue_wait_exceeds_cap"),
        "rejection must carry the cap code, got {body}"
    );
    assert!(
        body.contains("red.config.queue.max_wait_ms"),
        "rejection should name the cap config key for operators, got {body}"
    );
    assert!(
        body.contains("60000"),
        "rejection should name the active cap value, got {body}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "cap rejection must not park, elapsed={elapsed:?}"
    );
}

/// Issue #920 AC #2/#4 — server shutdown mid-wait surfaces a distinct
/// cancellation error to an in-flight RedWire waiter.
///
/// The waiter opens on an empty queue and parks. We then drive the
/// shutdown signal the way every sibling transport's smoke test does —
/// `QueueWaitRegistry::cancel_all()`, the sanctioned in-process analogue
/// of a server drain (AC #4: the same cancel that wakes the synchronous
/// condvar waiters wakes the async one). The waiter must emit a
/// `StreamError` carrying the `queue_wait_cancelled` code — a wire
/// outcome distinct from a `QueueEventPush` delivery and from the silent
/// no-frame timeout — echoing the open's correlation and stream so the
/// client pairs it with the wait it opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_shutdown_mid_wait_emits_distinct_cancellation_error() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    // Open a long wait on the empty queue — it will still be parked when
    // the shutdown signal fires.
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        91,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":5000}"#.to_vec(),
    )
    .with_stream(4);
    write_frame(&mut client, &open).await;

    // Give the session time to register its waiter and park, then drive
    // the shutdown drain through the registry.
    tokio::time::sleep(Duration::from_millis(150)).await;
    server.runtime.queue_wait_registry().cancel_all();

    let frame = read_frame(&mut client).await;
    assert_eq!(
        frame.kind,
        MessageKind::StreamError,
        "cancellation must surface as a StreamError, not a QueueEventPush"
    );
    assert_ne!(
        frame.kind,
        MessageKind::QueueEventPush,
        "a cancelled wait delivered no message"
    );
    assert_eq!(frame.stream_id, 4, "error echoes the wait's stream_id");
    assert_eq!(
        frame.correlation_id, 91,
        "error echoes the open correlation"
    );
    let body = String::from_utf8(frame.payload.clone()).expect("utf8 payload");
    assert!(
        body.contains("queue_wait_cancelled"),
        "cancellation code must be distinct from queue_wait_failed, got {body}"
    );
}

/// Issue #920 AC #1/#3 — closing the connection mid-wait aborts the
/// in-flight wait and releases its registry slot promptly, rather than
/// stranding the waiter (and a tokio worker) until the `wait_ms`
/// deadline.
///
/// Observability hook: `QueueWaitRegistry::live_waiters` counts the
/// `Arc<Slot>` references held by parked waiters on a key. The waiter
/// opens with a deliberately long `wait_ms`; once parked the count is 1.
/// After the client socket is dropped, the session's frame loop hits EOF
/// and returns, dropping the connection-scoped `JoinSet` and aborting
/// the wait — so the count must fall back to 0 far inside the wait
/// budget, distinct from a timeout that would only fire at the deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_close_mid_wait_releases_registry_slot() {
    let server = start_server().await;
    server
        .runtime
        .execute_query("CREATE QUEUE jobs")
        .expect("create queue");
    server
        .runtime
        .execute_query("QUEUE GROUP CREATE jobs workers")
        .expect("create group");

    let mut client = connect_and_handshake(server.addr).await;

    // A 30s budget: a release observed within a couple of seconds is
    // unambiguously the connection-close abort, not the wait deadline.
    let open = Frame::new(
        MessageKind::QueueWaitOpen,
        55,
        br#"{"queue":"jobs","group":"workers","consumer":"c1","count":1,"wait_ms":30000}"#.to_vec(),
    )
    .with_stream(6);
    write_frame(&mut client, &open).await;

    let registry = server.runtime.queue_wait_registry();

    // Wait for the session to park its waiter (live_waiters → 1). The
    // scope is the default (empty) tenant the RedWire path resolves to.
    let mut parked = false;
    for _ in 0..200 {
        if registry.live_waiters("", "jobs") == 1 {
            parked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(parked, "waiter should be parked before we close the socket");

    // Close the connection mid-wait.
    drop(client);

    // The slot reference must be released well before the 30s deadline.
    let mut released = false;
    for _ in 0..300 {
        if registry.live_waiters("", "jobs") == 0 {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        released,
        "closing the connection must abort the parked wait and release its slot promptly"
    );
}
