//! Issue #1044 / PRD #1041 — `red ui --server red://host` / `reds://host`
//! bridge to a *remote* RedWire endpoint.
//!
//! The local loopback RedWire-over-WS endpoint now fronts a remote
//! RedWire-over-TCP (and -TLS) connection instead of an embedded engine,
//! reusing the same byte-pump seam (ADR 0036 / 0047 / 0049). These tests
//! assert the load-bearing acceptance criteria:
//!
//!   1. `red://host:port` — the served page opens a RedWire-over-WS
//!      session against the local `127.0.0.1` WS endpoint and a query
//!      round-trips through the bridge to a running RedWire-over-TCP
//!      instance.
//!   2. `reds://host:port` — the same, with the bridge negotiating TLS to
//!      the target transparently (the WS client never speaks TLS).
//!   3. The UI talks only to the local loopback WS endpoint; the remote
//!      connection is owned by the bridge.
//!
//! The WS client here stands in for the browser: it speaks the binary
//! RedWire framing over a binary WebSocket exactly as the served fixture
//! page does.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reddb_server::server::header_escape_guard::HeaderEscapeGuard;
use reddb_server::server::ui_bridge::{
    spawn_ui_bridge_remote, RemoteRedwireTarget, UiBridgeConfig,
};
use reddb_server::wire::tls::generate_self_signed_cert;
use reddb_server::wire::WireTlsConfig;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_wire::redwire::{
    build_auth_response_anonymous_payload, build_auth_response_frame, build_client_hello_frame,
    build_query_frame, drain_next_frame, frame_to_bytes, Frame, MessageKind,
    MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC,
};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TIMEOUT: Duration = Duration::from_secs(20);

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// An in-memory runtime to back the remote RedWire listener under test.
fn remote_runtime() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime"))
}

/// Bind an ephemeral loopback port and serve plain RedWire-over-TCP on it.
async fn spawn_remote_tcp() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let addr = listener.local_addr().expect("addr");
    let rt = remote_runtime();
    tokio::spawn(async move {
        let _ = reddb_server::wire::start_redwire_listener_on(listener, rt).await;
    });
    addr
}

/// Bind an ephemeral loopback port and serve RedWire-over-TLS on it,
/// returning the bound address plus the self-signed cert PEM (used as the
/// bridge's trust anchor).
async fn spawn_remote_tls() -> (SocketAddr, Vec<u8>) {
    let (cert_pem, key_pem) = generate_self_signed_cert("localhost").expect("self-signed cert");
    let dir = tempfile::tempdir().expect("temp dir");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, &cert_pem).expect("write cert");
    std::fs::write(&key_path, &key_pem).expect("write key");
    let tls_config = WireTlsConfig {
        cert_path,
        key_path,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tls");
    let addr = listener.local_addr().expect("addr");
    let rt = remote_runtime();
    tokio::spawn(async move {
        // The TempDir must outlive the listener; move it into the task.
        let _dir = dir;
        let _ = reddb_server::wire::start_redwire_tls_listener_on(listener, rt, &tls_config).await;
    });
    (addr, cert_pem.into_bytes())
}

fn ws_request(
    url: &str,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut req = url.into_client_request().expect("client request");
    req.headers_mut().insert(
        "Origin",
        HeaderEscapeGuard::header_value(origin).expect("origin value"),
    );
    req
}

async fn send_frame(ws: &mut WsStream, frame: &Frame) {
    ws.send(Message::Binary(Bytes::from(frame_to_bytes(frame))))
        .await
        .expect("send frame");
}

/// Pull binary WS messages and reassemble the self-delimiting RedWire
/// frame stream, returning the next decoded frame.
async fn next_frame(ws: &mut WsStream, buf: &mut Vec<u8>) -> Frame {
    loop {
        if let Some(frame) = drain_next_frame(buf).expect("decode frame") {
            return frame;
        }
        let msg = timeout(TIMEOUT, ws.next())
            .await
            .expect("frame within budget")
            .expect("ws stream open")
            .expect("ws message");
        match msg {
            Message::Binary(bytes) => buf.extend_from_slice(&bytes),
            Message::Close(_) => panic!("ws closed before a full frame arrived"),
            _ => {}
        }
    }
}

/// Drive the anonymous RedWire handshake to completion over the WS — the
/// exact native preamble the browser sends, relayed through the bridge to
/// the remote listener.
async fn anonymous_handshake(ws: &mut WsStream, buf: &mut Vec<u8>) {
    ws.send(Message::Binary(Bytes::from(vec![
        REDWIRE_MAGIC,
        MAX_KNOWN_MINOR_VERSION,
    ])))
    .await
    .expect("send preamble");

    send_frame(
        ws,
        &build_client_hello_frame(1, ["anonymous"], 0, Some("ui-bridge-remote-e2e"))
            .expect("hello frame"),
    )
    .await;
    let ack = next_frame(ws, buf).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");

    send_frame(
        ws,
        &build_auth_response_frame(2, build_auth_response_anonymous_payload())
            .expect("auth response frame"),
    )
    .await;
    let ok = next_frame(ws, buf).await;
    assert_eq!(
        ok.kind,
        MessageKind::AuthOk,
        "anonymous handshake should AuthOk: {}",
        String::from_utf8_lossy(&ok.payload)
    );
}

/// Run a CREATE / INSERT / SELECT round-trip over the WS session, proving
/// the queries reached the remote engine through the bridge.
async fn assert_query_round_trip(ws: &mut WsStream, buf: &mut Vec<u8>) {
    send_frame(
        ws,
        &build_query_frame(10, "CREATE TABLE widgets (id INTEGER, name TEXT)")
            .expect("query frame"),
    )
    .await;
    let created = next_frame(ws, buf).await;
    assert_eq!(
        created.kind,
        MessageKind::Result,
        "create failed: {}",
        String::from_utf8_lossy(&created.payload)
    );

    send_frame(
        ws,
        &build_query_frame(11, "INSERT INTO widgets (id, name) VALUES (1, 'alpha')")
            .expect("query frame"),
    )
    .await;
    let inserted = next_frame(ws, buf).await;
    assert_eq!(inserted.kind, MessageKind::Result, "insert must Result");
    let insert_body = String::from_utf8_lossy(&inserted.payload);
    assert!(
        insert_body.contains("\"affected\":1"),
        "insert must report one affected row, got: {insert_body}"
    );

    send_frame(
        ws,
        &build_query_frame(12, "SELECT id, name FROM widgets WHERE id = 1").expect("query frame"),
    )
    .await;
    let selected = next_frame(ws, buf).await;
    assert_eq!(
        selected.kind,
        MessageKind::Result,
        "select must Result: {}",
        String::from_utf8_lossy(&selected.payload)
    );
    // The INSERT's `affected:1` (above) already proves the write landed in
    // the *remote* engine via the bridge; the SELECT completing as a
    // `Result` over the same session proves the read leg round-trips too.
}

// ====================================================================
// AC — red://host:port: query round-trips through the local WS endpoint
//      to a running RedWire-over-TCP instance.
// ====================================================================

#[tokio::test]
async fn bridge_runs_query_over_remote_tcp() {
    let remote = spawn_remote_tcp().await;

    let bridge = spawn_ui_bridge_remote(
        RemoteRedwireTarget {
            host: remote.ip().to_string(),
            port: remote.port(),
            tls: false,
            ca_pem: None,
        },
        UiBridgeConfig::default(),
    )
    .await
    .expect("bridge starts");

    // The UI only ever connects to the local loopback WS endpoint.
    let ws_url = bridge.ws_url();
    assert!(
        ws_url.starts_with(&format!("ws://127.0.0.1:{}", bridge.local_addr().port())),
        "the served WS endpoint must be loopback: {ws_url}"
    );

    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let (mut ws, _resp) = connect_async(ws_request(&ws_url, &origin))
        .await
        .expect("ws connects to loopback bridge");
    let mut buf = Vec::new();

    anonymous_handshake(&mut ws, &mut buf).await;
    assert_query_round_trip(&mut ws, &mut buf).await;

    let _ = ws.close(None).await;
    bridge.shutdown().await;
}

// ====================================================================
// AC — reds://host:port: same round-trip with the bridge negotiating TLS
//      to the target transparently.
// ====================================================================

#[tokio::test]
async fn bridge_runs_query_over_remote_tls() {
    let (remote, ca_pem) = spawn_remote_tls().await;

    let bridge = spawn_ui_bridge_remote(
        RemoteRedwireTarget {
            // 127.0.0.1 is a SAN on the generated self-signed cert.
            host: remote.ip().to_string(),
            port: remote.port(),
            tls: true,
            ca_pem: Some(ca_pem),
        },
        UiBridgeConfig::default(),
    )
    .await
    .expect("bridge starts");

    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    // The WS client speaks plain ws:// to the loopback bridge — it is
    // unaware the remote leg is TLS.
    let (mut ws, _resp) = connect_async(ws_request(&bridge.ws_url(), &origin))
        .await
        .expect("ws connects to loopback bridge");
    let mut buf = Vec::new();

    anonymous_handshake(&mut ws, &mut buf).await;
    assert_query_round_trip(&mut ws, &mut buf).await;

    let _ = ws.close(None).await;
    bridge.shutdown().await;
}
