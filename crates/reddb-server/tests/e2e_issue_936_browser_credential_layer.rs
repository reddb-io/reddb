//! Issue #936 / PRD #930 — browser credential layer, hybrid token.
//!
//! End-to-end coverage of the four acceptance criteria:
//!
//!   1. A browser auth flow yields an access JWT (returned in the body,
//!      kept in memory by the SPA) plus a refresh cookie that is
//!      `HttpOnly`, `Secure`, and `SameSite` — `POST /auth/browser/login`.
//!   2. The RedWire WS handshake accepts a valid access token and rejects
//!      expired / invalid ones — the `oauth-jwt` handshake method carrying
//!      the access JWT.
//!   3. Access-token rotation does not tear down in-flight streams: a
//!      session authenticated with an access token keeps serving queries
//!      and *opens new streams* even after that access token has expired,
//!      because the bearer authenticates only the handshake/open and the
//!      stream lease (ADR 0029) governs delivery thereafter.
//!   4. mTLS stays native-only — the browser endpoint has no
//!      client-certificate path (structural guard).
//!
//! The HTTP edge here is the clear-text background server (the cookie's
//! `Secure` attribute is still emitted — the test inspects the header, it
//! does not require a TLS socket to do so). In production the browser
//! endpoint is WSS/HTTPS-only (ADR 0036); that gating is exercised by the
//! issue-#935 WS-edge tests and is orthogonal to the credential shape.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::time::Duration;

use reddb_server::auth::browser_token::{
    BrowserIdentity, BrowserTokenAuthority, BrowserTokenConfig,
};
use reddb_server::auth::{AuthConfig, AuthStore, Role};
use reddb_server::server::RedDBServer;
use reddb_server::wire::redwire::{
    decode_frame, encode_frame, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};
use reddb_server::{RedDBOptions, RedDBRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(20);
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn build_authority(access_ttl_secs: i64) -> Arc<BrowserTokenAuthority> {
    let mut cfg = BrowserTokenConfig::new(SECRET.to_vec());
    cfg.access_ttl_secs = access_ttl_secs;
    Arc::new(BrowserTokenAuthority::new(cfg).expect("authority builds"))
}

/// A runtime with auth enabled, one admin user, and the browser-token
/// authority wired in. Returned as an `Arc` so it can back both the HTTP
/// edge and the RedWire listener (they share the same inner state).
fn build_runtime(authority: Arc<BrowserTokenAuthority>) -> Arc<RedDBRuntime> {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        ..Default::default()
    }));
    store.create_user("alice", "secret", Role::Admin).unwrap();
    runtime.set_auth_store(Arc::clone(&store));
    runtime.set_browser_token_authority(Some(authority));
    Arc::new(runtime)
}

/// A runtime with the browser-token authority wired but auth otherwise
/// open. Used by the in-flight-stream test, whose property (a session
/// surviving its access token's expiry) is independent of whether the
/// AuthStore is enabled — keeping the data plane unconditionally open
/// isolates the test from any orthogonal policy gating.
fn build_runtime_open(authority: Arc<BrowserTokenAuthority>) -> Arc<RedDBRuntime> {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    runtime.set_browser_token_authority(Some(authority));
    Arc::new(runtime)
}

// --------------------------------------------------------------------
// Minimal blocking HTTP/1.1 client (the SPA's fetch, in Rust).
// --------------------------------------------------------------------

struct HttpReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn http_post(addr: SocketAddr, path: &str, body: &str, cookie: Option<&str>) -> HttpReply {
    let mut stream = StdTcpStream::connect(addr).expect("connect http");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let cookie_line = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{cookie_line}Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");

    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    HttpReply {
        status,
        headers,
        body: body.to_string(),
    }
}

fn json_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    // Tiny extractor for `"key":"value"` — sufficient for the flat
    // response envelopes here without pulling a JSON dep into the test.
    let needle = format!("\"{key}\":\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

// --------------------------------------------------------------------
// RedWire handshake driver (the browser's WS data channel, over TCP).
// --------------------------------------------------------------------

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
    decode_frame(&buf).expect("decode frame").0
}

async fn write_frame(stream: &mut TcpStream, frame: &Frame) {
    stream
        .write_all(&encode_frame(frame))
        .await
        .expect("write frame");
}

/// Drive the `oauth-jwt` handshake to completion with `jwt`. Returns the
/// connected stream (still open) plus the terminal handshake frame
/// (`AuthOk` or `AuthFail`).
async fn oauth_jwt_handshake(addr: SocketAddr, jwt: &str) -> (TcpStream, Frame) {
    let mut stream = TcpStream::connect(addr).await.expect("connect redwire");
    stream.write_all(&[REDWIRE_MAGIC]).await.unwrap();
    stream.write_all(&[MAX_KNOWN_MINOR_VERSION]).await.unwrap();
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::Hello,
            1,
            br#"{"versions":[1],"auth_methods":["oauth-jwt"],"features":0,"client_name":"browser-cred-e2e"}"#.to_vec(),
        ),
    )
    .await;
    let ack = read_frame(&mut stream).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");

    let auth = format!("{{\"jwt\":\"{jwt}\"}}");
    write_frame(
        &mut stream,
        &Frame::new(MessageKind::AuthResponse, 2, auth.into_bytes()),
    )
    .await;
    let terminal = read_frame(&mut stream).await;
    (stream, terminal)
}

async fn start_redwire(runtime: Arc<RedDBRuntime>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = reddb_server::wire::redwire::start_redwire_listener_on(listener, runtime).await;
    });
    addr
}

fn start_http(runtime: &RedDBRuntime) -> SocketAddr {
    let server = RedDBServer::new(runtime.clone());
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind http");
    let addr = listener.local_addr().unwrap();
    let _ = server.serve_in_background_on(listener);
    addr
}

// ====================================================================
// AC #1 — login yields access JWT + HttpOnly/Secure/SameSite refresh
//          cookie; refresh rotates; logout clears.
// ====================================================================

#[test]
fn browser_login_issues_access_jwt_and_secure_refresh_cookie() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(authority);
    let addr = start_http(&runtime);

    let reply = http_post(
        addr,
        "/auth/browser/login",
        r#"{"username":"alice","password":"secret"}"#,
        None,
    );
    assert_eq!(reply.status, 200, "login body: {}", reply.body);

    // Access JWT in the body (the SPA holds it in memory).
    let access = json_field(&reply.body, "access_token").expect("access_token present");
    assert!(access.split('.').count() == 3, "access_token is a JWT");
    assert!(reply.body.contains("\"token_type\":\"Bearer\""));
    assert!(reply.body.contains("\"role\":\"admin\""));

    // Refresh cookie: HttpOnly + Secure + SameSite, never readable by JS.
    let cookie = reply.header("set-cookie").expect("Set-Cookie present");
    assert!(cookie.contains("reddb_refresh="), "cookie: {cookie}");
    assert!(
        cookie.contains("HttpOnly"),
        "cookie must be HttpOnly: {cookie}"
    );
    assert!(cookie.contains("Secure"), "cookie must be Secure: {cookie}");
    assert!(
        cookie.contains("SameSite="),
        "cookie must set SameSite: {cookie}"
    );
    // The access token must NOT appear in any cookie — it lives in memory.
    assert!(
        !cookie.contains(access),
        "access token must not be in a cookie"
    );
}

#[test]
fn browser_login_rejects_bad_password() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(authority);
    let addr = start_http(&runtime);

    let reply = http_post(
        addr,
        "/auth/browser/login",
        r#"{"username":"alice","password":"wrong"}"#,
        None,
    );
    assert_eq!(
        reply.status, 401,
        "bad password must be 401: {}",
        reply.body
    );
    assert!(
        reply.header("set-cookie").is_none(),
        "no cookie on failed login"
    );
}

#[test]
fn browser_refresh_rotates_access_and_cookie_then_logout_clears() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(authority);
    let addr = start_http(&runtime);

    let login = http_post(
        addr,
        "/auth/browser/login",
        r#"{"username":"alice","password":"secret"}"#,
        None,
    );
    assert_eq!(login.status, 200);
    let set_cookie = login.header("set-cookie").expect("cookie").to_string();
    // Reduce the Set-Cookie to the `name=value` pair the browser echoes.
    let cookie_pair = set_cookie.split(';').next().unwrap().trim().to_string();
    let first_access = json_field(&login.body, "access_token").unwrap().to_string();

    // Tokens encode `iat`/`exp` at one-second granularity and HS256 is
    // deterministic, so a refresh within the same wall-clock second
    // reproduces the same access JWT. Wait past the second boundary so
    // the rotation is observable as a genuinely distinct token.
    std::thread::sleep(Duration::from_millis(1100));

    // Refresh with the cookie → a fresh access token + a rotated cookie.
    let refresh = http_post(addr, "/auth/browser/refresh", "", Some(&cookie_pair));
    assert_eq!(refresh.status, 200, "refresh body: {}", refresh.body);
    let second_access = json_field(&refresh.body, "access_token").expect("refreshed access");
    assert_ne!(
        first_access, second_access,
        "refresh must mint a new access token"
    );
    assert!(
        refresh
            .header("set-cookie")
            .is_some_and(|c| c.contains("reddb_refresh=")),
        "refresh must rotate the cookie"
    );

    // Refresh without the cookie → 401.
    let no_cookie = http_post(addr, "/auth/browser/refresh", "", None);
    assert_eq!(no_cookie.status, 401);

    // Logout clears the cookie (Max-Age=0).
    let logout = http_post(addr, "/auth/browser/logout", "", Some(&cookie_pair));
    assert_eq!(logout.status, 200);
    let cleared = logout
        .header("set-cookie")
        .expect("logout sets a clearing cookie");
    assert!(cleared.contains("Max-Age=0"), "logout cookie: {cleared}");
}

// ====================================================================
// AC #2 — WS handshake accepts a valid access token, rejects expired /
//          invalid ones.
// ====================================================================

#[tokio::test]
async fn ws_handshake_accepts_valid_access_token() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(Arc::clone(&authority));
    let addr = start_redwire(Arc::clone(&runtime)).await;

    let identity = BrowserIdentity {
        username: "alice".to_string(),
        tenant: Some("acme".to_string()),
        role: Role::Admin,
    };
    let access = authority
        .issue(&identity, now_unix_secs())
        .unwrap()
        .access_token;

    let (_stream, frame) = oauth_jwt_handshake(addr, &access).await;
    assert_eq!(
        frame.kind,
        MessageKind::AuthOk,
        "valid access token should AuthOk: {}",
        String::from_utf8_lossy(&frame.payload)
    );
    assert!(String::from_utf8_lossy(&frame.payload).contains("alice"));
}

#[tokio::test]
async fn ws_handshake_rejects_expired_access_token() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(Arc::clone(&authority));
    let addr = start_redwire(Arc::clone(&runtime)).await;

    let identity = BrowserIdentity {
        username: "alice".to_string(),
        tenant: None,
        role: Role::Admin,
    };
    // Issue as-of far in the past so exp is well behind the server's now.
    let expired = authority
        .issue(&identity, now_unix_secs() - 100_000)
        .unwrap()
        .access_token;

    let (_stream, frame) = oauth_jwt_handshake(addr, &expired).await;
    assert_eq!(
        frame.kind,
        MessageKind::AuthFail,
        "expired token must AuthFail"
    );
}

#[tokio::test]
async fn ws_handshake_rejects_garbage_and_refresh_tokens() {
    let authority = build_authority(15 * 60);
    let runtime = build_runtime(Arc::clone(&authority));
    let addr = start_redwire(Arc::clone(&runtime)).await;

    // Structurally invalid token.
    let (_s1, garbage) = oauth_jwt_handshake(addr, "not.a.jwt").await;
    assert_eq!(garbage.kind, MessageKind::AuthFail);

    // A *valid* refresh token must not authenticate a session — only an
    // access token may (defence against refresh-token replay on the WS).
    let identity = BrowserIdentity {
        username: "alice".to_string(),
        tenant: None,
        role: Role::Admin,
    };
    let refresh = authority
        .issue(&identity, now_unix_secs())
        .unwrap()
        .refresh_token;
    let (_s2, frame) = oauth_jwt_handshake(addr, &refresh).await;
    assert_eq!(
        frame.kind,
        MessageKind::AuthFail,
        "a refresh token must never authenticate a WS session"
    );
}

// ====================================================================
// AC #3 — access-token rotation does not tear down in-flight work.
//          A session authenticated with an access token keeps serving
//          queries and opens new streams even after that token expires
//          (the bearer authenticates only the handshake/open; the stream
//          lease — ADR 0029 — governs delivery thereafter).
// ====================================================================

#[tokio::test]
async fn established_session_survives_access_token_expiry() {
    // Short access TTL so it expires while the connection stays live.
    let authority = build_authority(2);
    let runtime = build_runtime_open(Arc::clone(&authority));
    let addr = start_redwire(Arc::clone(&runtime)).await;

    let identity = BrowserIdentity {
        username: "alice".to_string(),
        tenant: None,
        role: Role::Admin,
    };
    let access = authority
        .issue(&identity, now_unix_secs())
        .unwrap()
        .access_token;

    // Handshake while the token is still valid.
    let (mut stream, ok) = oauth_jwt_handshake(addr, &access).await;
    assert_eq!(ok.kind, MessageKind::AuthOk);

    // Seed a table on the live session.
    for sql in [
        "CREATE TABLE widgets (id INTEGER, name TEXT)",
        "INSERT INTO widgets (id, name) VALUES (1, 'a')",
        "INSERT INTO widgets (id, name) VALUES (2, 'b')",
    ] {
        write_frame(
            &mut stream,
            &Frame::new(MessageKind::Query, 10, sql.as_bytes().to_vec()),
        )
        .await;
        let reply = read_frame(&mut stream).await;
        assert_eq!(
            reply.kind,
            MessageKind::Result,
            "setup query failed: {}",
            String::from_utf8_lossy(&reply.payload)
        );
    }

    // Let the access token expire (TTL = 2s).
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        authority.validate_access(&access, now_unix_secs()).is_err(),
        "precondition: the access token is now expired"
    );

    // The established session still serves a query — auth was the
    // handshake, not each frame.
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::Query,
            11,
            b"SELECT id, name FROM widgets".to_vec(),
        ),
    )
    .await;
    let q = read_frame(&mut stream).await;
    assert_eq!(
        q.kind,
        MessageKind::Result,
        "query after token expiry must still succeed"
    );

    // And it still *opens a new stream* after expiry: OpenAck → chunk(s)
    // → StreamEnd. This is the in-flight-stream guarantee — rotating the
    // browser's access token never disturbs work on the live connection.
    write_frame(
        &mut stream,
        &Frame::new(
            MessageKind::OpenStream,
            12,
            br#"{"sql":"SELECT id, name FROM widgets"}"#.to_vec(),
        )
        .with_stream(1),
    )
    .await;

    let ack = read_frame(&mut stream).await;
    assert_eq!(
        ack.kind,
        MessageKind::OpenAck,
        "stream must open post-expiry: {}",
        String::from_utf8_lossy(&ack.payload)
    );

    // Drain to the terminal StreamEnd.
    let mut saw_chunk = false;
    loop {
        let frame = read_frame(&mut stream).await;
        match frame.kind {
            MessageKind::StreamChunk => saw_chunk = true,
            MessageKind::StreamEnd => break,
            MessageKind::StreamError => {
                panic!(
                    "stream errored post-expiry: {}",
                    String::from_utf8_lossy(&frame.payload)
                )
            }
            other => panic!("unexpected stream frame {other:?}"),
        }
    }
    assert!(
        saw_chunk,
        "stream delivered at least one chunk after token expiry"
    );
}

// ====================================================================
// AC #4 — mTLS stays native-only: the browser WS edge has no client-
//          certificate path. Structural guard over the edge source.
// ====================================================================

#[test]
fn browser_ws_edge_has_no_mtls_path() {
    // The browser credential layer authenticates exclusively via the
    // bearer/OAuth-JWT handshake (the access token). mTLS is a native
    // transport concern (ADR 0036) and must never leak into the WS edge.
    let ws_edge = include_str!("../src/server/ws_edge.rs");
    for forbidden in [
        "client_cert",
        "peer_cert",
        "client_auth",
        "ClientCert",
        "mTLS client",
    ] {
        assert!(
            !ws_edge.contains(forbidden),
            "ws_edge must not reference a browser client-certificate / mTLS path ({forbidden})"
        );
    }
}
