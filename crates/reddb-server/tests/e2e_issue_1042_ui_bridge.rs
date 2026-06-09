//! Issue #1042 / PRD #1041 — `red ui --server file://<path>` bridge.
//!
//! End-to-end coverage of the bridge's machine-verifiable acceptance
//! criteria, exercising the same RedWire-over-WebSocket path the served
//! page uses (prior art: the RedWire conformance / browser-credential
//! WS tests, here driven by a Rust WS client):
//!
//!   1. The bridge boots over a temporary `.rdb`, serves the UI bundle,
//!      and a RedWire-over-WebSocket session opened against the **embedded
//!      engine** runs a query against the file database and gets a result.
//!   2. The local WS endpoint enforces the default-deny Origin allowlist
//!      (ADR 0036): a foreign or missing Origin is refused the upgrade,
//!      while the bridge's own served loopback origin is accepted.
//!   3. The bridge shuts down cleanly on request with no orphaned task.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reddb_server::server::ui_bridge::{spawn_ui_bridge, UiBridgeConfig};
use reddb_server::server::RedDBServer;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_wire::redwire::{
    build_auth_response_frame, build_client_hello_frame, build_query_frame, decode_error_payload,
    decode_frame, decode_query_result_payload, encode_frame, Frame, MessageKind,
    MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC,
};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

const SUBPROTOCOL: &str = "reddb.redwire.v1";

/// Open the embedded engine on a temp `.rdb` and build a server over it.
fn embedded_server(dir: &TempDir) -> RedDBServer {
    let path = dir.path().join("ui-bridge.rdb");
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(
        path.to_str().expect("temp path utf-8"),
    ))
    .expect("embedded runtime opens");
    RedDBServer::new(runtime)
}

/// A RedWire-over-WebSocket client: the browser's data channel, in Rust.
/// Sends frame bytes as binary WS messages and reassembles inbound binary
/// messages into RedWire frames (framing is self-delimiting).
struct WsConn {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    inbox: Vec<u8>,
}

impl WsConn {
    /// Connect to `ws_url` carrying `origin` as the `Origin` header and
    /// the RedWire subprotocol. Returns an error if the upgrade is
    /// refused (the gate's default-deny path).
    async fn connect(ws_url: &str, origin: Option<&str>) -> Result<Self, String> {
        let mut request = ws_url
            .into_client_request()
            .map_err(|e| format!("build request: {e}"))?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        if let Some(origin) = origin {
            request.headers_mut().insert(
                "Origin",
                HeaderValue::from_str(origin).map_err(|e| format!("origin header: {e}"))?,
            );
        }
        let (stream, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("ws connect: {e}"))?;
        Ok(Self {
            stream,
            inbox: Vec::new(),
        })
    }

    async fn send_bytes(&mut self, bytes: Vec<u8>) {
        self.stream
            .send(Message::Binary(Bytes::from(bytes)))
            .await
            .expect("ws send");
    }

    async fn send_frame(&mut self, frame: &Frame) {
        self.send_bytes(encode_frame(frame)).await;
    }

    /// Read the next complete RedWire frame, pulling more binary messages
    /// off the socket until the inbox holds a full frame.
    async fn read_frame(&mut self) -> Frame {
        loop {
            match decode_frame(&self.inbox) {
                Ok((frame, consumed)) => {
                    self.inbox.drain(..consumed);
                    return frame;
                }
                Err(_) => {
                    // Need more bytes.
                    let msg = self
                        .stream
                        .next()
                        .await
                        .expect("ws stream not closed")
                        .expect("ws message ok");
                    match msg {
                        Message::Binary(b) => self.inbox.extend_from_slice(&b),
                        Message::Close(_) => panic!("ws closed before a full frame arrived"),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Drive the anonymous handshake to AuthOk: magic+version preamble,
    /// Hello, AuthResponse(empty). Mirrors the browser codec.
    async fn handshake(&mut self) {
        self.send_bytes(vec![REDWIRE_MAGIC, MAX_KNOWN_MINOR_VERSION])
            .await;
        self.send_frame(
            &build_client_hello_frame(1, ["anonymous", "bearer"], 0, Some("red-ui-e2e"))
                .expect("hello frame"),
        )
        .await;
        let ack = self.read_frame().await;
        assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");

        self.send_frame(&build_auth_response_frame(2, Vec::new()).expect("auth response"))
            .await;
        let ok = self.read_frame().await;
        assert_eq!(
            ok.kind,
            MessageKind::AuthOk,
            "anonymous handshake should AuthOk: {}",
            String::from_utf8_lossy(&ok.payload)
        );
    }

    /// Run a plain query and return the decoded Result JSON, panicking on
    /// an Error frame.
    async fn query(&mut self, corr: u64, sql: &str) -> serde_json::Value {
        self.send_frame(&build_query_frame(corr, sql).expect("query frame"))
            .await;
        let reply = self.read_frame().await;
        match reply.kind {
            MessageKind::Result => {
                decode_query_result_payload(&reply.payload).expect("result payload is JSON")
            }
            MessageKind::Error => {
                panic!(
                    "query `{sql}` errored: {}",
                    decode_error_payload(&reply.payload)
                )
            }
            other => panic!("unexpected reply kind {other:?} for `{sql}`"),
        }
    }
}

// ====================================================================
// AC: boot over a temp .rdb, open a RedWire-over-WS session over the
//     embedded engine, and assert a query result.
// ====================================================================

#[tokio::test]
async fn ws_session_runs_query_against_file_database() {
    let dir = TempDir::new().unwrap();
    let server = embedded_server(&dir);
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge binds");

    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let mut conn = WsConn::connect(&bridge.ws_url(), Some(&origin))
        .await
        .expect("ws upgrade accepted for served origin");

    conn.handshake().await;

    // DDL + DML against the embedded engine opened from the file.
    let created = conn
        .query(10, "CREATE TABLE widgets (id INTEGER, name TEXT)")
        .await;
    assert_eq!(created["ok"], serde_json::Value::Bool(true));

    let inserted = conn
        .query(11, "INSERT INTO widgets (id, name) VALUES (1, 'a')")
        .await;
    assert_eq!(inserted["ok"], serde_json::Value::Bool(true));
    assert_eq!(inserted["affected"], serde_json::json!(1));

    // The read path returns a result against the file database.
    let selected = conn.query(12, "SELECT id, name FROM widgets").await;
    assert_eq!(selected["ok"], serde_json::Value::Bool(true));

    bridge.shutdown().await;
}

// ====================================================================
// AC: the local WS endpoint enforces the default-deny Origin allowlist.
// ====================================================================

#[tokio::test]
async fn ws_endpoint_enforces_default_deny_origin_allowlist() {
    let dir = TempDir::new().unwrap();
    let server = embedded_server(&dir);
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge binds");

    // A foreign origin is refused the upgrade.
    let foreign = WsConn::connect(&bridge.ws_url(), Some("https://evil.example.com")).await;
    assert!(
        foreign.is_err(),
        "foreign origin must be refused the WS upgrade"
    );

    // A missing Origin is refused (a browser always sends one).
    let no_origin = WsConn::connect(&bridge.ws_url(), None).await;
    assert!(no_origin.is_err(), "missing origin must be refused");

    // The bridge's own served loopback origin is accepted and reaches a
    // working session.
    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let mut allowed = WsConn::connect(&bridge.ws_url(), Some(&origin))
        .await
        .expect("served loopback origin is accepted");
    allowed.handshake().await;

    bridge.shutdown().await;
}

// ====================================================================
// AC: the bridge serves the UI bundle (fixture) over local HTTP.
// ====================================================================

#[tokio::test]
async fn bridge_serves_ui_fixture_over_http() {
    let dir = TempDir::new().unwrap();
    let server = embedded_server(&dir);
    let bridge = spawn_ui_bridge(server, UiBridgeConfig::default())
        .await
        .expect("bridge binds");

    let index = http_get(&bridge.ui_url()).await;
    assert!(index.contains("<!DOCTYPE html>"), "index is HTML: {index}");
    assert!(
        index.contains("redwire-core.js"),
        "index loads the browser codec"
    );

    // The codec the page imports is served alongside it.
    let core_url = format!("http://{}/redwire-core.js", bridge.local_addr());
    let core = http_get(&core_url).await;
    assert!(
        core.contains("connectRedwireOverSocket"),
        "redwire-core.js is served"
    );

    bridge.shutdown().await;
}

// ====================================================================
// AC: closing / interrupting the command shuts the bridge down cleanly.
// ====================================================================

#[tokio::test]
async fn bridge_shuts_down_cleanly_freeing_the_port() {
    let dir = TempDir::new().unwrap();
    let server = embedded_server(&dir);
    let bridge = spawn_ui_bridge(
        server,
        UiBridgeConfig {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ui_dir: None,
            extra_allowed_origins: Vec::new(),
        },
    )
    .await
    .expect("bridge binds");
    let addr = bridge.local_addr();

    bridge.shutdown().await;

    // After a clean shutdown the listener has released the port, so we
    // can bind it again (no orphaned listener task).
    let rebind = tokio::net::TcpListener::bind(addr).await;
    assert!(
        rebind.is_ok(),
        "port {addr} should be free after clean shutdown"
    );
}

/// Minimal HTTP/1.1 GET returning the response body (the bridge serves
/// `Connection: close`-style short responses fine over one round-trip).
async fn http_get(url: &str) -> String {
    // url is `http://host:port/...`
    let without_scheme = url.strip_prefix("http://").expect("http url");
    let (authority, path) = match without_scheme.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{p}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(&authority)
        .await
        .expect("connect http");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw);
    let (_head, body) = text
        .split_once("\r\n\r\n")
        .expect("response has header/body split");
    body.to_string()
}

/// Sanity: the canonicalizer turns a relative `file://` into an absolute
/// `file:///…` (also unit-tested in the module; asserted here at the
/// integration boundary for the AC).
#[test]
fn relative_file_uri_canonicalizes_to_absolute() {
    let out = reddb_server::server::ui_bridge::canonicalize_file_uri("file://./x.rdb").unwrap();
    assert!(out.starts_with("file:///"), "must be absolute: {out}");
    assert!(out.ends_with("/x.rdb"), "keeps the file name: {out}");
}
