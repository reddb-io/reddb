//! Issue #1047 / PRD #1041 — `red server --ui` serves the pinned red-ui
//! bundle on the server's HTTP surface, alongside the API, with opt-in
//! network reach governed by the bind address (ADR 0050 distribution,
//! ADR 0051 exposure model, ADR 0049 transport).
//!
//! These tests exercise the served-bundle surface through a real HTTP
//! round-trip against `RedDBServer` (the same edge the CLI runs), with the
//! bundle directory attached via `with_ui_dir` — the seam `red server
//! --ui` / `--ui-dir` wires from the CLI. They pin the four acceptance
//! criteria:
//!
//!   1. The bundle is served on the server HTTP surface (`GET /` →
//!      `index.html`, named assets with the right content type).
//!   2. Reach follows the bind address: the bundle is served on exactly
//!      the HTTP listener's bound socket — no separate UI port — so the
//!      OS bind (default loopback) governs who can reach it.
//!   3. The served assets carry no credential, and an authed database
//!      still requires the browser to authenticate on the data endpoint
//!      (the inert assets stay public; `POST /query` does not).
//!   4. Without `--ui` the surface is API-only — `GET /` is the discovery
//!      document and unknown asset paths 404.

use reddb_server::auth::{AuthConfig, AuthStore, Role};
use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// A throwaway red-ui bundle on disk: an index page plus a couple of
/// assets. The bytes are deliberately credential-free — the test asserts
/// they reach the client verbatim.
fn write_bundle() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("index.html"),
        b"<!doctype html><html><head></head><body>red-ui</body></html>",
    )
    .unwrap();
    std::fs::write(dir.path().join("app.js"), b"console.log('red-ui boot')").unwrap();
    std::fs::create_dir(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("assets/style.css"), b"body{margin:0}").unwrap();
    dir
}

struct ServerHandle {
    addr: SocketAddr,
    _join: thread::JoinHandle<std::io::Result<()>>,
}

fn start_server(server: RedDBServer) -> ServerHandle {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let addr = listener.local_addr().expect("server addr");
    let join = server.serve_in_background_on(listener);
    ServerHandle { addr, _join: join }
}

struct HttpReply {
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// Minimal blocking `GET` (the browser's asset fetch, in Rust). Returns
/// the status, the `Content-Type` header, and the raw body bytes.
fn http_get(addr: SocketAddr, path: &str, bearer: Option<&str>) -> HttpReply {
    let auth_line = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth_line}Connection: close\r\n\r\n");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    parse_reply(&raw)
}

/// Minimal blocking `POST` used only to probe the data endpoint's auth.
fn http_post(addr: SocketAddr, path: &str, body: &str) -> HttpReply {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    parse_reply(&raw)
}

fn parse_reply(raw: &[u8]) -> HttpReply {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http framing");
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status");
    let content_type = head
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        })
        .map(|(_, v)| v.trim().to_string());
    HttpReply {
        status,
        content_type,
        body,
    }
}

/// AC #1 — `red server --ui` serves the bundle on the server HTTP surface:
/// the root yields `index.html` and named assets carry the right type.
#[test]
fn serves_bundle_root_and_assets_on_http_surface() {
    let bundle = write_bundle();
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let server = RedDBServer::new(runtime).with_ui_dir(bundle.path().to_path_buf());
    let handle = start_server(server);

    let index = http_get(handle.addr, "/", None);
    assert_eq!(index.status, 200, "GET / serves the bundle index");
    assert_eq!(
        index.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        index.body,
        b"<!doctype html><html><head></head><body>red-ui</body></html>"
    );

    let js = http_get(handle.addr, "/app.js", None);
    assert_eq!(js.status, 200);
    assert_eq!(
        js.content_type.as_deref(),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(js.body, b"console.log('red-ui boot')");

    let css = http_get(handle.addr, "/assets/style.css", None);
    assert_eq!(css.status, 200);
    assert_eq!(css.content_type.as_deref(), Some("text/css; charset=utf-8"));

    // A path that is neither an API route nor a bundle file still 404s —
    // the static surface is a fallback, not a catch-all 200.
    let missing = http_get(handle.addr, "/does-not-exist.js", None);
    assert_eq!(missing.status, 404);

    // The API still answers on the same surface — the bundle did not
    // shadow it. `/health/live` is unauthenticated by contract.
    let health = http_get(handle.addr, "/health/live", None);
    assert_eq!(health.status, 200);
}

/// AC #2 — network reach follows the bind address. The bundle is served on
/// exactly the HTTP listener's bound socket (loopback here, by default);
/// `--ui` opens no separate listener, so the OS bind governs reach. A
/// non-localhost bind would be the explicit opt-in to reach from another
/// host.
#[test]
fn reach_follows_the_bind_address() {
    let bundle = write_bundle();
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let server = RedDBServer::new(runtime).with_ui_dir(bundle.path().to_path_buf());
    let handle = start_server(server);

    // Default bind is loopback — reachable from the local host only.
    assert!(
        handle.addr.ip().is_loopback(),
        "default bind keeps the UI localhost-only: {}",
        handle.addr
    );

    // The bundle is served on that same bound socket (no separate UI port).
    let index = http_get(handle.addr, "/", None);
    assert_eq!(index.status, 200);
    assert_eq!(index.body, write_bundle_index());
}

fn write_bundle_index() -> Vec<u8> {
    b"<!doctype html><html><head></head><body>red-ui</body></html>".to_vec()
}

/// AC #3 — served assets carry no credential, and an authed database still
/// requires the browser to authenticate on the data endpoint. The inert
/// bundle is public (so the SPA can load), but `POST /query` is gated.
#[test]
fn assets_are_public_but_data_endpoint_requires_auth() {
    let bundle = write_bundle();
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..Default::default()
    }));
    store.create_user("alice", "secret", Role::Admin).unwrap();
    runtime.set_auth_store(Arc::clone(&store));
    let server = RedDBServer::new(runtime)
        .with_auth(store)
        .with_ui_dir(bundle.path().to_path_buf());
    let handle = start_server(server);

    // The inert bundle loads without a token (no credential embedded).
    let index = http_get(handle.addr, "/", None);
    assert_eq!(index.status, 200, "inert assets are public on an authed DB");
    let js = http_get(handle.addr, "/app.js", None);
    assert_eq!(js.status, 200);
    // The served bytes are exactly the bundle file — nothing injected, no
    // credential leaked into the asset path.
    assert_eq!(js.body, b"console.log('red-ui boot')");
    assert!(
        !String::from_utf8_lossy(&index.body)
            .to_lowercase()
            .contains("secret"),
        "served asset must not carry any credential"
    );

    // The data endpoint still demands authentication — the auth lives on
    // the data plane, not the asset path.
    let query = http_post(handle.addr, "/query", "{\"query\":\"SELECT 1\"}");
    assert_eq!(
        query.status, 401,
        "unauthenticated query must be rejected on an authed DB"
    );
}

/// AC #4 — without `--ui` the surface is API-only: `GET /` is the
/// self-describing discovery document (JSON), not an HTML bundle, and an
/// asset-shaped path 404s.
#[test]
fn without_ui_flag_root_is_api_discovery() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let server = RedDBServer::new(runtime); // no with_ui_dir
    let handle = start_server(server);

    let root = http_get(handle.addr, "/", None);
    assert_eq!(root.status, 200);
    assert_eq!(
        root.content_type.as_deref(),
        Some("application/json"),
        "API-only root stays the discovery document"
    );

    let asset = http_get(handle.addr, "/app.js", None);
    assert_eq!(asset.status, 404, "no bundle is served without --ui");
}
