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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

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
        _join: join,
    }
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
