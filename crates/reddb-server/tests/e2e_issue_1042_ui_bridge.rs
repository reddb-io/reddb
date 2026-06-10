//! Issue #1042 / PRD #1041 — `red ui --server file://<path>` tracer bullet.
//!
//! Boots the loopback RedWire-over-WebSocket bridge over a temporary `.rdb`,
//! drives a real WebSocket client (tokio-tungstenite) through the anonymous
//! RedWire handshake, runs a query against the embedded engine, and asserts
//! the result. Also covers the default-deny Origin gate (ADR 0036, loopback
//! variant) and clean session-scoped shutdown.
//!
//! The WebSocket client mirrors the on-wire framing of
//! crates/reddb-wire/src/redwire — the same primitives the native drivers and
//! the `tests/e2e_issue_936_browser_credential_layer.rs` TCP path use, just
//! carried over binary WS messages instead of a TCP byte stream.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use reddb_server::api::RedDBOptions;
use reddb_server::server::ui_bridge::{spawn_ui_bridge, UiBridge, UiBridgeConfig};
use reddb_server::{RedDBServer, ServerOptions};

use reddb_wire::redwire::{
    build_auth_response_anonymous_payload, build_auth_response_frame, build_client_hello_frame,
    build_open_stream_frame, build_query_frame, decode_frame, encode_frame, frame_len_from_header,
    Frame, MessageKind, OpenStreamRequest, FRAME_HEADER_SIZE, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a temporary file-backed engine and start a loopback UI bridge over it.
async fn boot_bridge() -> (UiBridge, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("ui-1042.rdb");
    let server = RedDBServer::from_database_options(
        RedDBOptions::persistent(&db_path),
        ServerOptions::default(),
    )
    .expect("open embedded engine");
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("spawn bridge");
    (bridge, tmp)
}

/// Connect a WebSocket client to the bridge's `/redwire` endpoint, optionally
/// stamping an `Origin` header (a browser always sends one).
async fn connect_ws(
    ws_url: &str,
    origin: Option<&str>,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = ws_url.into_client_request().expect("ws request");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "reddb.redwire.v1".parse().unwrap(),
    );
    if let Some(value) = origin {
        request
            .headers_mut()
            .insert("origin", value.parse().unwrap());
    }
    let (stream, _resp) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

async fn ws_send_frame(ws: &mut WsStream, frame: &Frame) {
    ws.send(Message::Binary(encode_frame(frame).into()))
        .await
        .expect("send frame");
}

/// Read exactly one RedWire frame, reassembling it from one or more binary WS
/// messages (RedWire's 16-byte length header makes the stream self-delimiting).
async fn ws_read_frame(ws: &mut WsStream, buf: &mut Vec<u8>) -> Frame {
    loop {
        if buf.len() >= FRAME_HEADER_SIZE {
            let header: [u8; FRAME_HEADER_SIZE] = buf[..FRAME_HEADER_SIZE].try_into().unwrap();
            if let Ok(total) = frame_len_from_header(&header) {
                if buf.len() >= total {
                    let (frame, consumed) = decode_frame(&buf[..total]).expect("decode frame");
                    buf.drain(..consumed);
                    return frame;
                }
            }
        }
        match timeout(EXCHANGE_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => buf.extend_from_slice(&bytes),
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => panic!("ws closed before a full frame"),
            Ok(Some(Ok(other))) => panic!("unexpected ws message: {other:?}"),
            Ok(Some(Err(err))) => panic!("ws error: {err}"),
            Err(_) => panic!("timed out waiting for a frame"),
        }
    }
}

/// Preamble + anonymous handshake → leaves the session ready for queries.
async fn handshake_anonymous(ws: &mut WsStream, buf: &mut Vec<u8>) {
    ws.send(Message::Binary(
        vec![REDWIRE_MAGIC, MAX_KNOWN_MINOR_VERSION].into(),
    ))
    .await
    .expect("send preamble");

    ws_send_frame(
        ws,
        &build_client_hello_frame(1, ["anonymous", "bearer"], 0, Some("ui-1042-e2e"))
            .expect("hello"),
    )
    .await;
    let ack = ws_read_frame(ws, buf).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");

    ws_send_frame(
        ws,
        &build_auth_response_frame(2, build_auth_response_anonymous_payload()).expect("auth resp"),
    )
    .await;
    let ok = ws_read_frame(ws, buf).await;
    assert_eq!(
        ok.kind,
        MessageKind::AuthOk,
        "anonymous auth must succeed: {}",
        String::from_utf8_lossy(&ok.payload)
    );
}

#[tokio::test]
async fn ui_bridge_runs_a_query_over_redwire_ws_against_the_embedded_engine() {
    let (bridge, _tmp) = boot_bridge().await;
    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());

    let mut ws = connect_ws(&bridge.ws_url(), Some(&origin))
        .await
        .expect("ws connects with an allowed origin");
    let mut buf = Vec::new();
    handshake_anonymous(&mut ws, &mut buf).await;

    for sql in [
        "CREATE TABLE widgets (id INTEGER, name TEXT)",
        "INSERT INTO widgets (id, name) VALUES (1, 'alice')",
    ] {
        ws_send_frame(&mut ws, &build_query_frame(10, sql).expect("query")).await;
        let reply = ws_read_frame(&mut ws, &mut buf).await;
        assert_eq!(
            reply.kind,
            MessageKind::Result,
            "setup query failed: {}",
            String::from_utf8_lossy(&reply.payload)
        );
    }

    // SELECT row data rides the streaming path (OpenStream → StreamChunk* →
    // StreamEnd); the buffered Query reply only summarizes. Chunks carry the
    // rows as JSON (see wire/redwire/output_stream.rs).
    ws_send_frame(
        &mut ws,
        &build_open_stream_frame(
            11,
            1,
            &OpenStreamRequest {
                sql: "SELECT id, name FROM widgets".to_string(),
                opts_raw: Vec::new(),
            },
        )
        .expect("open stream"),
    )
    .await;

    let ack = ws_read_frame(&mut ws, &mut buf).await;
    assert_eq!(
        ack.kind,
        MessageKind::OpenAck,
        "stream must open: {}",
        String::from_utf8_lossy(&ack.payload)
    );

    let mut chunk_bytes = Vec::new();
    let mut saw_chunk = false;
    loop {
        let frame = ws_read_frame(&mut ws, &mut buf).await;
        match frame.kind {
            MessageKind::StreamChunk => {
                saw_chunk = true;
                chunk_bytes.extend_from_slice(&frame.payload);
            }
            MessageKind::StreamEnd => break,
            MessageKind::StreamError => {
                panic!(
                    "stream errored: {}",
                    String::from_utf8_lossy(&frame.payload)
                )
            }
            other => panic!("unexpected stream frame {other:?}"),
        }
    }
    assert!(saw_chunk, "the query must deliver at least one row chunk");

    let rendered = String::from_utf8_lossy(&chunk_bytes);
    assert!(
        rendered.contains("alice"),
        "streamed rows must carry the inserted value, got: {rendered}"
    );

    bridge.shutdown().await;
}

#[tokio::test]
async fn ui_bridge_default_denies_a_missing_origin() {
    // A WS upgrade with no Origin header (non-browser caller) is refused by
    // the loopback gate — default-deny, ADR 0036.
    let (bridge, _tmp) = boot_bridge().await;
    let result = connect_ws(&bridge.ws_url(), None).await;
    assert!(
        result.is_err(),
        "a missing Origin must be rejected by the loopback gate"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn ui_bridge_rejects_a_foreign_origin() {
    // A cross-site Origin (CSWSH attempt) is refused even though the bridge
    // speaks plain ws:// on loopback.
    let (bridge, _tmp) = boot_bridge().await;
    let result = connect_ws(&bridge.ws_url(), Some("http://evil.example.com")).await;
    assert!(
        result.is_err(),
        "a foreign Origin must be rejected by the loopback gate"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn ui_bridge_serves_the_ui_bundle_over_http() {
    let (bridge, _tmp) = boot_bridge().await;
    let body = timeout(EXCHANGE_TIMEOUT, async { http_get(&bridge.ui_url()).await })
        .await
        .expect("http fetch within budget");
    assert!(
        body.contains("RedDB") && body.contains("/redwire"),
        "served page should be the UI fixture pointing at /redwire"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn ui_bridge_shutdown_releases_the_port() {
    let (bridge, _tmp) = boot_bridge().await;
    let ws_url = bridge.ws_url();
    let port = bridge.local_addr().port();
    let origin = format!("http://127.0.0.1:{port}");

    // Sanity: it serves before shutdown.
    let ws = connect_ws(&ws_url, Some(&origin))
        .await
        .expect("connects while live");
    drop(ws);

    bridge.shutdown().await;

    // After shutdown the endpoint is gone: a fresh connect must fail.
    let after = connect_ws(&ws_url, Some(&origin)).await;
    assert!(
        after.is_err(),
        "the bridge must stop serving once shut down (no orphaned listener)"
    );
}

/// Tiny HTTP GET over a raw TCP socket — avoids pulling a heavier HTTP client
/// into the test just to read one static page.
async fn http_get(url: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let mut stream = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("connect");
    let request = format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    String::from_utf8_lossy(&response).into_owned()
}
