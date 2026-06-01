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
