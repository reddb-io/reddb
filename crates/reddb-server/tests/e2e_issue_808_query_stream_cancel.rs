//! Issue #808 / PRD #750 (750d) — `/query/stream` cancellation + tombstone.
//!
//! Fourth tracer-bullet of the #750 split: explicit cancellation and
//! disconnect handling for long-running reads. These tests pin the wire
//! contract end-to-end through `RedDBServer` over real HTTP:
//!
//!   1. `POST /query/stream/cancel` with a cursor token (same tenant +
//!      principal) returns `200 {"ok":true,"status":"cancelled"}` and
//!      tombstones the cursor.
//!   2. Resuming a cancelled cursor is refused to its owner with
//!      `409 cursor_cancelled` — distinct from expiry (410) and
//!      unknown/foreign (404).
//!   3. A cancel for a token owned by a different tenant is masked as
//!      `404 cursor_not_found` (no existence leak) and leaves the rightful
//!      owner's cursor resumable.
//!   4. An unknown token cancels to `404 cursor_not_found`.
//!   5. A body without a cursor is refused with `400 cursor_required`.
//!   6. Cancel is idempotent — a second cancel still returns `200`.

use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::thread;
use std::time::Duration;

struct ServerHandle {
    addr: std::net::SocketAddr,
    _server: RedDBServer,
    _join: thread::JoinHandle<std::io::Result<()>>,
}

fn start_server() -> ServerHandle {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let server = RedDBServer::new(runtime);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("server addr");
    let join = server.serve_in_background_on(listener);
    ServerHandle {
        addr,
        _server: server,
        _join: join,
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn request_raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> Vec<u8> {
    let mut headers = String::new();
    for (name, value) in extra_headers {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut out = Vec::new();
    stream.read_to_end(&mut out).expect("read response");
    out
}

/// Buffered request returning `(status, body)`.
fn request_collect(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let raw = request_raw(addr, method, path, extra_headers, body);
    let response = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = response.split_once("\r\n\r\n").expect("http framing");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("parse status");
    (status, body.to_string())
}

fn post_query(addr: std::net::SocketAddr, sql: &str) -> (u16, String) {
    let body = format!("{{\"query\": {}}}", json_string(sql));
    request_collect(addr, "POST", "/query", &[], &body)
}

fn post_stream(addr: std::net::SocketAddr, body: &str, extra_headers: &[(&str, &str)]) -> Vec<u8> {
    request_raw(addr, "POST", "/query/stream", extra_headers, body)
}

fn post_cancel(
    addr: std::net::SocketAddr,
    body: &str,
    extra_headers: &[(&str, &str)],
) -> (u16, String) {
    request_collect(addr, "POST", "/query/stream/cancel", extra_headers, body)
}

fn split_head_body(raw: &[u8]) -> (String, Vec<u8>) {
    let sep = b"\r\n\r\n";
    let pos = raw
        .windows(sep.len())
        .position(|w| w == sep)
        .expect("http header/body separator");
    let head = String::from_utf8_lossy(&raw[..pos]).into_owned();
    let body = raw[pos + sep.len()..].to_vec();
    (head, body)
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut i = 0usize;
    loop {
        let line_end = body[i..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .expect("chunk size line")
            + i;
        let size_str = String::from_utf8_lossy(&body[i..line_end]);
        let size = usize::from_str_radix(size_str.trim(), 16).expect("hex chunk size");
        i = line_end + 2;
        if size == 0 {
            break;
        }
        payload.extend_from_slice(&body[i..i + size]);
        i += size + 2;
    }
    payload
}

fn stream_lines(raw: &[u8]) -> (String, Vec<String>) {
    let (head, body) = split_head_body(raw);
    let payload = dechunk(&body);
    let text = String::from_utf8(payload).expect("utf8 ndjson");
    let lines = text
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (head, lines)
}

fn cursor_token(lines: &[String]) -> String {
    let frame = lines
        .iter()
        .find(|l| l.starts_with("{\"cursor\":"))
        .expect("a cursor control frame must be present");
    let needle = "\"token\":\"";
    let start = frame.find(needle).expect("cursor frame carries a token") + needle.len();
    let rest = &frame[start..];
    let end = rest.find('"').expect("token string terminator");
    rest[..end].to_string()
}

fn seed_users(addr: std::net::SocketAddr) {
    let (s, b) = post_query(addr, "CREATE TABLE users (id INT, name TEXT)");
    assert_eq!(s, 200, "create table: {b}");
    let (s, b) = post_query(
        addr,
        "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_eq!(s, 200, "insert: {b}");
}

const TENANT_ACME: (&str, &str) = ("x-reddb-tenant", "acme");
const TENANT_EVIL: (&str, &str) = ("x-reddb-tenant", "evil-corp");
const PRINCIPAL_ALICE: (&str, &str) = ("Authorization", "Bearer alice-token");

/// Open a fresh stream as acme/alice and return its cursor token.
fn open_cursor(addr: std::net::SocketAddr) -> String {
    let raw = post_stream(
        addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("SELECT id, name FROM users")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, lines) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "open head=\n{head}");
    cursor_token(&lines)
}

#[test]
fn explicit_cancel_tombstones_and_resume_is_refused_409() {
    let h = start_server();
    seed_users(h.addr);
    let token = open_cursor(h.addr);

    // Cancel under the owning scope — structured 200.
    let (status, body) = post_cancel(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    assert_eq!(status, 200, "owner cancel succeeds: {body}");
    assert!(
        body.contains("\"status\":\"cancelled\""),
        "cancel envelope: {body}"
    );

    // Resuming a tombstoned cursor is refused with a dedicated reason.
    let (status, body) = request_collect(
        h.addr,
        "POST",
        "/query/stream",
        &[TENANT_ACME, PRINCIPAL_ALICE],
        &format!("{{\"cursor\": {}}}", json_string(&token)),
    );
    assert_eq!(status, 409, "cancelled cursor refuses resume: {body}");
    assert!(
        body.contains("cursor_cancelled"),
        "resume of a cancelled cursor is cursor_cancelled: {body}"
    );
}

#[test]
fn cancel_unknown_token_is_masked_as_not_found() {
    let h = start_server();
    seed_users(h.addr);
    let (status, body) = post_cancel(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string("deadbeefdeadbeef")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    assert_eq!(status, 404, "unknown token cancels to 404: {body}");
    assert!(body.contains("cursor_not_found"), "masked: {body}");
}

#[test]
fn cancel_from_a_different_tenant_is_masked_and_owner_unaffected() {
    let h = start_server();
    seed_users(h.addr);
    let token = open_cursor(h.addr);

    // A foreign tenant cannot cancel — masked identically to unknown.
    let (status, body) = post_cancel(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_EVIL, PRINCIPAL_ALICE],
    );
    assert_eq!(status, 404, "cross-tenant cancel masked: {body}");
    assert!(
        body.contains("cursor_not_found"),
        "no existence leak: {body}"
    );

    // The rightful owner's cursor is untouched — still resumable.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, lines) = stream_lines(&raw);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "owner resume after foreign cancel still works: {head}"
    );
    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(rows, 2, "pinned rows still re-stream: {lines:?}");
}

#[test]
fn cancel_without_a_cursor_is_400() {
    let h = start_server();
    seed_users(h.addr);
    let (status, body) = post_cancel(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM users")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    assert_eq!(status, 400, "missing cursor is a 400: {body}");
    assert!(
        body.contains("cursor_required"),
        "structured reason: {body}"
    );
}

#[test]
fn cancel_is_idempotent() {
    let h = start_server();
    seed_users(h.addr);
    let token = open_cursor(h.addr);

    for attempt in 1..=2 {
        let (status, body) = post_cancel(
            h.addr,
            &format!("{{\"cursor\": {}}}", json_string(&token)),
            &[TENANT_ACME, PRINCIPAL_ALICE],
        );
        assert_eq!(status, 200, "cancel attempt {attempt} returns 200: {body}");
        assert!(
            body.contains("\"status\":\"cancelled\""),
            "envelope: {body}"
        );
    }
}
