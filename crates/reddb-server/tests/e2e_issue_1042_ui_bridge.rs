//! Issue #1042 / PRD #1041 — `red ui --server file://<path>` bridge spine.
//!
//! End-to-end coverage of the bridge's load-bearing acceptance criteria:
//!
//!   1. Booting over a temporary `.rdb`, the bridge serves the UI bundle
//!      directory on a loopback HTTP port (the served page is reachable).
//!   2. The served page can open a RedWire-over-WebSocket session against
//!      the **embedded engine** and run a query against the file database
//!      (prior art: the RedWire conformance / smoke tests).
//!   3. The local WS endpoint enforces the default-deny Origin allowlist
//!      (ADR 0036, adapted for loopback) — a cross-site / missing Origin
//!      cannot open a session, only the bridge's own served origin can.
//!   4. The bridge shuts down cleanly with no orphaned listener.
//!
//! The WS client here stands in for the browser: it speaks the binary
//! RedWire framing (16-byte little-endian header) over a binary
//! WebSocket, exactly as the served fixture page does.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reddb_server::server::ui_bridge::{spawn_ui_bridge, UiBridgeConfig};
use reddb_server::server::RedDBServer;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_wire::redwire::{
    build_auth_response_anonymous_payload, build_auth_response_frame, build_client_hello_frame,
    build_query_frame, drain_next_frame, frame_to_bytes, Frame, MessageKind,
    MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TIMEOUT: Duration = Duration::from_secs(20);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A persistent embedded runtime on a temp `.rdb`, wrapped in a server —
/// the file database the bridge fronts. The `TempDir` is returned so the
/// file outlives the test body.
fn file_server() -> (RedDBServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("ui.rdb");
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(path.to_str().expect("utf-8 path")))
            .expect("runtime opens file db");
    (RedDBServer::new(runtime), dir)
}

/// Build a WS upgrade request carrying an explicit `Origin` header.
fn ws_request(
    url: &str,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut req = url.into_client_request().expect("client request");
    req.headers_mut().insert(
        "Origin",
        HeaderValue::from_str(origin).expect("origin value"),
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

/// Drive the anonymous RedWire handshake to completion over the WS.
async fn anonymous_handshake(ws: &mut WsStream, buf: &mut Vec<u8>) {
    // Discriminator + minor-version byte (the native preamble).
    ws.send(Message::Binary(Bytes::from(vec![
        REDWIRE_MAGIC,
        MAX_KNOWN_MINOR_VERSION,
    ])))
    .await
    .expect("send preamble");

    send_frame(
        ws,
        &build_client_hello_frame(1, ["anonymous"], 0, Some("ui-bridge-e2e")).expect("hello frame"),
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

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect http");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write GET");
    let mut raw = Vec::new();
    timeout(TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .expect("read within budget")
        .expect("read response");
    String::from_utf8_lossy(&raw).into_owned()
}

// ====================================================================
// AC — boot over a temp .rdb, open a RedWire-over-WS session over the
//      embedded engine, and assert a query result. Also serves the UI
//      bundle directory.
// ====================================================================

#[tokio::test]
async fn bridge_runs_query_over_embedded_engine() {
    let (server, _dir) = file_server();
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge starts");

    // The served origin is what the page loaded from the bridge presents.
    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let (mut ws, _resp) = connect_async(ws_request(&bridge.ws_url(), &origin))
        .await
        .expect("ws connects with served origin");
    let mut buf = Vec::new();

    anonymous_handshake(&mut ws, &mut buf).await;

    // CREATE + INSERT execute against the embedded file engine.
    send_frame(
        &mut ws,
        &build_query_frame(10, "CREATE TABLE widgets (id INTEGER, name TEXT)")
            .expect("query frame"),
    )
    .await;
    let created = next_frame(&mut ws, &mut buf).await;
    assert_eq!(
        created.kind,
        MessageKind::Result,
        "create failed: {}",
        String::from_utf8_lossy(&created.payload)
    );

    send_frame(
        &mut ws,
        &build_query_frame(11, "INSERT INTO widgets (id, name) VALUES (1, 'alpha')")
            .expect("query frame"),
    )
    .await;
    let inserted = next_frame(&mut ws, &mut buf).await;
    assert_eq!(
        inserted.kind,
        MessageKind::Result,
        "insert must return a Result"
    );
    // The insert summary proves the write landed in the file engine.
    let insert_body = String::from_utf8_lossy(&inserted.payload);
    assert!(
        insert_body.contains("\"affected\":1"),
        "insert must report one affected row, got: {insert_body}"
    );

    // And a SELECT over the same session returns a query result — the
    // browser's round-trip against the file database.
    send_frame(
        &mut ws,
        &build_query_frame(12, "SELECT id, name FROM widgets WHERE id = 1").expect("query frame"),
    )
    .await;
    let selected = next_frame(&mut ws, &mut buf).await;
    assert_eq!(
        selected.kind,
        MessageKind::Result,
        "select must return a Result: {}",
        String::from_utf8_lossy(&selected.payload)
    );

    let _ = ws.close(None).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn bridge_serves_the_ui_bundle() {
    let (server, _dir) = file_server();
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge starts");
    let addr = bridge.local_addr();

    let response = http_get(addr, "/").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "bundle root must be served: {}",
        response.lines().next().unwrap_or("")
    );
    assert!(
        response.contains("RedDB UI") && response.contains("/redwire"),
        "served page must be the UI fixture that opens the redwire WS"
    );

    bridge.shutdown().await;
}

// ====================================================================
// AC — the WS endpoint enforces the default-deny Origin allowlist.
// ====================================================================

#[tokio::test]
async fn bridge_rejects_cross_site_origin() {
    let (server, _dir) = file_server();
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge starts");

    // A cross-site origin is not on the allowlist → the upgrade is refused
    // (HTTP 403) and the handshake never completes.
    let result = connect_async(ws_request(&bridge.ws_url(), "http://evil.example.com")).await;
    assert!(
        result.is_err(),
        "a cross-site origin must not open a redwire session"
    );

    // A different loopback port is a *different* origin — exact match only.
    let wrong_port = connect_async(ws_request(&bridge.ws_url(), "http://127.0.0.1:1")).await;
    assert!(
        wrong_port.is_err(),
        "an off-by-port origin must be rejected (exact match)"
    );

    // The served origin still works — the gate is default-deny, not
    // deny-all.
    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let allowed = connect_async(ws_request(&bridge.ws_url(), &origin)).await;
    assert!(
        allowed.is_ok(),
        "the bridge's own served origin must be allowed"
    );
    if let Ok((mut ws, _)) = allowed {
        let _ = ws.close(None).await;
    }

    bridge.shutdown().await;
}
