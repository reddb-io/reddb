//! Issue #1048 / PRD #1041 — `red ui` credential handoff.
//!
//! End-to-end coverage of the load-bearing acceptance criteria:
//!
//!   1. `red ui <uri> --token <X>` presents `<X>` in the RedWire handshake
//!      (ADR 0036 bearer) and the UI opens already authenticated **without
//!      the UI ever sending the token** — the WS client here drives an
//!      anonymous handshake (no credential) and still gets `AuthOk`, proving
//!      the bridge injected the held bearer token on its behalf.
//!   2. The token never appears in anything the UI is served (the page config
//!      is credential-free) — asserted alongside the auth-mode hint.
//!   3. The deep-link/desktop secret channel hands the credential over a
//!      one-time, nonce-keyed loopback fetch; the handoff URL carries the
//!      nonce, not the secret, and a replay gets nothing.
//!   4. The database's auth configuration is the source of truth: an
//!      authenticated DB with no token serves `auth mode = prompt`; an
//!      unauthenticated DB serves `auth mode = open`.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reddb_server::auth::{AuthConfig, AuthStore, Role};
use reddb_server::server::ui_auth::{spawn_handoff_server, UiAuthMode};
use reddb_server::server::ui_bridge::{spawn_ui_bridge, UiBridgeConfig};
use reddb_server::server::RedDBServer;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_wire::redwire::{
    build_auth_response_anonymous_payload, build_auth_response_frame, build_client_hello_frame,
    build_query_frame, decode_frame, encode_frame, Frame, MessageKind, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TIMEOUT: Duration = Duration::from_secs(20);
const TEST_TOKEN_NAME: &str = "ui-handoff";

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// An embedded runtime on a temp `.rdb` with auth **enabled**, one admin
/// user, and an API key. Returns the server plus the bearer token string
/// (`rk_…`) so the bridge can hold it. The `TempDir` outlives the body.
fn authenticated_file_server() -> (RedDBServer, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("ui.rdb");
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(path.to_str().expect("utf-8 path")))
            .expect("runtime opens file db");
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        ..Default::default()
    }));
    store
        .create_user("alice", "secret", Role::Admin)
        .expect("create user");
    let key = store
        .create_api_key("alice", TEST_TOKEN_NAME, Role::Admin)
        .expect("create api key");
    runtime.set_auth_store(Arc::clone(&store));
    (RedDBServer::new(runtime), key.key, dir)
}

/// An embedded runtime with auth **disabled** (no store).
fn unauthenticated_file_server() -> (RedDBServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("ui.rdb");
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(path.to_str().expect("utf-8 path")))
            .expect("runtime opens file db");
    (RedDBServer::new(runtime), dir)
}

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
    ws.send(Message::Binary(Bytes::from(encode_frame(frame))))
        .await
        .expect("send frame");
}

async fn next_frame(ws: &mut WsStream, buf: &mut Vec<u8>) -> Frame {
    loop {
        if buf.len() >= reddb_wire::redwire::FRAME_HEADER_SIZE {
            if let Ok((frame, consumed)) = decode_frame(buf) {
                buf.drain(..consumed);
                return frame;
            }
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

/// Drive a **credential-free** handshake from the UI's side: the client
/// advertises only `anonymous` and sends an anonymous AuthResponse — it never
/// holds or sends a token. In injected-auth mode the bridge swaps this for a
/// bearer AuthResponse carrying the held token toward the engine.
async fn ui_handshake_without_token(ws: &mut WsStream, buf: &mut Vec<u8>) -> Frame {
    ws.send(Message::Binary(Bytes::from(vec![
        REDWIRE_MAGIC,
        MAX_KNOWN_MINOR_VERSION,
    ])))
    .await
    .expect("send preamble");

    send_frame(
        ws,
        &build_client_hello_frame(1, ["anonymous"], 0, Some("ui-1048-e2e")).expect("hello frame"),
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
    next_frame(ws, buf).await
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
// AC #1 / #5 — the held token is presented in the handshake; the UI
//      opens authenticated without ever sending the token.
// ====================================================================

#[tokio::test]
async fn injected_auth_opens_session_without_ui_sending_token() {
    let (server, token, _dir) = authenticated_file_server();
    let config = UiBridgeConfig {
        injected_token: Some(token.clone()),
        auth_mode: UiAuthMode::Injected,
        ..Default::default()
    };
    let bridge = spawn_ui_bridge(server, config)
        .await
        .expect("bridge starts");

    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let (mut ws, _resp) = connect_async(ws_request(&bridge.ws_url(), &origin))
        .await
        .expect("ws connects");
    let mut buf = Vec::new();

    // The UI never sends the token — yet the session authenticates, because
    // `red` injected the held bearer credential into the handshake.
    let auth_reply = ui_handshake_without_token(&mut ws, &mut buf).await;
    assert_eq!(
        auth_reply.kind,
        MessageKind::AuthOk,
        "injected bearer must authenticate against the auth-enabled DB; got {:?}: {}",
        auth_reply.kind,
        String::from_utf8_lossy(&auth_reply.payload)
    );
    // The AuthOk identifies the admin principal the token belongs to — proof
    // the bearer token (not anonymous) was what authenticated.
    let body = String::from_utf8_lossy(&auth_reply.payload);
    assert!(
        body.contains("alice"),
        "session must be the token's principal (alice), got: {body}"
    );

    // And the authenticated session can write against the engine.
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
        "create over the injected-auth session failed: {}",
        String::from_utf8_lossy(&created.payload)
    );

    let _ = ws.close(None).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn auth_enabled_db_rejects_anonymous_without_injection() {
    // Control: with no injected token, the same auth-enabled DB refuses the
    // UI's anonymous handshake — proving the AuthOk above came from injection,
    // not from the DB being open.
    let (server, _token, _dir) = authenticated_file_server();
    let config = UiBridgeConfig {
        injected_token: None,
        auth_mode: UiAuthMode::Prompt,
        ..Default::default()
    };
    let bridge = spawn_ui_bridge(server, config)
        .await
        .expect("bridge starts");

    let origin = format!("http://127.0.0.1:{}", bridge.local_addr().port());
    let (mut ws, _resp) = connect_async(ws_request(&bridge.ws_url(), &origin))
        .await
        .expect("ws connects");
    let mut buf = Vec::new();

    // Drive the UI's credential-free handshake directly: the auth-enabled DB
    // has no overlapping auth method for an `anonymous`-only client, so it
    // refuses with AuthFail (at the Hello stage, before any HelloAck).
    ws.send(Message::Binary(Bytes::from(vec![
        REDWIRE_MAGIC,
        MAX_KNOWN_MINOR_VERSION,
    ])))
    .await
    .expect("send preamble");
    send_frame(
        &mut ws,
        &build_client_hello_frame(1, ["anonymous"], 0, Some("ui-1048-e2e")).expect("hello frame"),
    )
    .await;
    let reply = next_frame(&mut ws, &mut buf).await;
    assert_eq!(
        reply.kind,
        MessageKind::AuthFail,
        "anonymous handshake against an auth-enabled DB must fail without injection; got {:?}",
        reply.kind
    );

    let _ = ws.close(None).await;
    bridge.shutdown().await;
}

// ====================================================================
// AC #2 — the served page is credential-free (no token leaks into HTML).
// ====================================================================

#[tokio::test]
async fn served_page_never_contains_the_token() {
    let (server, token, _dir) = authenticated_file_server();
    let config = UiBridgeConfig {
        injected_token: Some(token.clone()),
        auth_mode: UiAuthMode::Injected,
        ..Default::default()
    };
    let bridge = spawn_ui_bridge(server, config)
        .await
        .expect("bridge starts");
    let addr = bridge.local_addr();

    let response = http_get(addr, "/").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "page must be served: {}",
        response.lines().next().unwrap_or("")
    );
    assert!(
        !response.contains(&token),
        "the served page must NOT contain the bearer token"
    );
    assert!(
        response.contains("window.REDDB_AUTH_MODE=\"injected\""),
        "injected-auth pages advertise the injected mode (credential-free)"
    );

    bridge.shutdown().await;
}

// ====================================================================
// AC #4 — the DB's auth config drives whether the UI prompts.
// ====================================================================

#[tokio::test]
async fn authenticated_db_without_token_serves_prompt_mode() {
    let (server, _token, _dir) = authenticated_file_server();
    let config = UiBridgeConfig {
        injected_token: None,
        auth_mode: UiAuthMode::Prompt,
        ..Default::default()
    };
    let bridge = spawn_ui_bridge(server, config)
        .await
        .expect("bridge starts");
    let addr = bridge.local_addr();

    let response = http_get(addr, "/").await;
    assert!(
        response.contains("window.REDDB_AUTH_MODE=\"prompt\""),
        "an authenticated DB with no token must prompt"
    );

    bridge.shutdown().await;
}

#[tokio::test]
async fn unauthenticated_db_serves_open_mode() {
    let (server, _dir) = unauthenticated_file_server();
    let config = UiBridgeConfig {
        injected_token: None,
        auth_mode: UiAuthMode::Open,
        ..Default::default()
    };
    let bridge = spawn_ui_bridge(server, config)
        .await
        .expect("bridge starts");
    let addr = bridge.local_addr();

    let response = http_get(addr, "/").await;
    assert!(
        response.contains("window.REDDB_AUTH_MODE=\"open\""),
        "an unauthenticated DB must not prompt"
    );

    bridge.shutdown().await;
}

// ====================================================================
// AC #3 — the deep-link/desktop secret channel: one-time, nonce-keyed,
//      and the handoff URL carries the nonce not the secret.
// ====================================================================

#[tokio::test]
async fn handoff_server_yields_token_once_via_nonce_url() {
    let token = "rk_desktop_handoff_secret".to_string();
    let handoff = spawn_handoff_server(token.clone())
        .await
        .expect("handoff server starts");

    let url = handoff.handoff_url();
    // The URL carries the nonce, never the secret.
    assert!(
        !url.contains(&token),
        "handoff URL must not contain the secret: {url}"
    );
    assert!(
        url.contains("/handoff/"),
        "handoff URL is the nonce-keyed path: {url}"
    );

    // First fetch returns the credential.
    let path = url.split(&handoff.local_addr().to_string()).nth(1).unwrap();
    let first = http_get(handoff.local_addr(), path).await;
    assert!(
        first.starts_with("HTTP/1.1 200"),
        "first fetch 200: {first}"
    );
    assert!(first.contains(&token), "first fetch returns the token body");
    assert!(
        handoff.is_consumed(),
        "the secret is consumed after one fetch"
    );

    // A replay (or any second fetch) gets nothing.
    let second = http_get(handoff.local_addr(), path).await;
    assert!(
        second.starts_with("HTTP/1.1 404"),
        "second fetch must be 404 (single-use): {second}"
    );
    assert!(!second.contains(&token), "a replay must not leak the token");

    // A wrong nonce never reveals the secret.
    let wrong = http_get(handoff.local_addr(), "/handoff/deadbeef").await;
    assert!(wrong.starts_with("HTTP/1.1 404"), "wrong nonce is 404");

    handoff.shutdown().await;
}
