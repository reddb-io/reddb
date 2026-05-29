//! Issue #807 / PRD #750 (750c) — `/query/stream` cursor registry.
//!
//! Third tracer-bullet of the #750 split: a server-side cursor that lets a
//! client resume or reference a streamed read. These tests pin the wire
//! contract end-to-end through `RedDBServer` over real HTTP:
//!
//!   1. The stream prelude carries an opaque `cursor` control frame
//!      (token + snapshot pin + TTL) right after the descriptor.
//!   2. Resuming with `{"cursor":"<token>"}` (same tenant + principal)
//!      re-streams the pinned view — happy-path resume.
//!   3. A token presented by a different tenant is refused with a uniform
//!      `404 cursor_not_found` that does not confirm the token exists —
//!      tenant isolation with no existence leak.
//!   4. A token presented by a different principal is likewise masked as
//!      `404 cursor_not_found` — principal isolation.
//!   5. After the cursor's TTL elapses, the rightful owner's resume is
//!      refused with `410 cursor_expired` — TTL expiry.

use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
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
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Buffered POST /query — returns `(status, body)`.
fn post_query(addr: std::net::SocketAddr, sql: &str) -> (u16, String) {
    let body = format!("{{\"query\": {}}}", json_string(sql));
    request_collect(addr, "POST", "/query", &[], &body)
}

/// PUT /config/{key} with a scalar `{"value": ...}` body.
fn put_config(addr: std::net::SocketAddr, key: &str, value_json: &str) -> (u16, String) {
    let body = format!("{{\"value\": {value_json}}}");
    request_collect(addr, "PUT", &format!("/config/{key}"), &[], &body)
}

/// Generic request that reads the whole response to a string.
fn request_collect(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let raw = request_raw(addr, method, path, extra_headers, body, None, None);
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

/// Raw request. `extra_headers` are appended verbatim (used for
/// `x-reddb-tenant` and `Authorization`). Returns the full raw response.
#[allow(clippy::too_many_arguments)]
fn request_raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
    read_chunk_size: Option<usize>,
    inter_read_delay: Option<Duration>,
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
    match read_chunk_size {
        None => {
            stream.read_to_end(&mut out).expect("read response");
        }
        Some(size) => {
            let mut buf = vec![0u8; size];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        if let Some(delay) = inter_read_delay {
                            thread::sleep(delay);
                        }
                    }
                    Err(err) => panic!("read error: {err}"),
                }
            }
        }
    }
    out
}

/// POST /query/stream with optional tenant/principal headers and a raw
/// JSON body (used both for fresh opens and `{"cursor":...}` resumes).
fn post_stream(addr: std::net::SocketAddr, body: &str, extra_headers: &[(&str, &str)]) -> Vec<u8> {
    request_raw(
        addr,
        "POST",
        "/query/stream",
        extra_headers,
        body,
        None,
        None,
    )
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

/// Decode the chunked NDJSON body into `(head, trimmed frame lines)`.
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

/// Pull the opaque cursor token out of the `{"cursor":{...}}` control
/// frame. Treats the value as opaque — just lifts the quoted string after
/// `"token":`.
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
const PRINCIPAL_MALLORY: (&str, &str) = ("Authorization", "Bearer mallory-token");

#[test]
fn stream_prelude_carries_an_opaque_cursor_after_the_descriptor() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream(
        h.addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("SELECT id, name FROM users")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, lines) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "head=\n{head}");

    // Descriptor stays first (frozen #750 wire shape), cursor is the next
    // early control frame.
    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "descriptor must remain the first frame: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("{\"cursor\":"),
        "cursor control frame must follow the descriptor: {}",
        lines[1]
    );
    let cursor_frame = &lines[1];
    assert!(
        cursor_frame.contains("\"token\":\"")
            && cursor_frame.contains("\"snapshot_lsn\":")
            && cursor_frame.contains("\"ttl_ms\":")
            && cursor_frame.contains("\"expires_at_ms\":"),
        "cursor frame must carry token + snapshot pin + TTL: {cursor_frame}"
    );
    // Token is opaque (hex), not a snapshot LSN echo.
    let token = cursor_token(&lines);
    assert_eq!(token.len(), 48, "opaque 192-bit hex token: {token}");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "hex: {token}");

    // Rows + terminal still arrive after the cursor frame.
    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(
        rows, 2,
        "rows still stream after the cursor frame: {lines:?}"
    );
    assert!(
        lines.last().unwrap().contains("\"row_count\":2"),
        "terminal frame intact: {}",
        lines.last().unwrap()
    );
}

#[test]
fn resume_against_unexpired_cursor_restreams_the_pinned_view() {
    let h = start_server();
    seed_users(h.addr);

    // Open and grab the cursor.
    let raw = post_stream(
        h.addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("SELECT id, name FROM users")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (_head, lines) = stream_lines(&raw);
    let token = cursor_token(&lines);

    // Resume with ONLY the cursor — no query — same tenant + principal.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, lines) = stream_lines(&raw);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "resume must succeed, head=\n{head}"
    );
    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "resume re-streams a full descriptor-first stream: {}",
        lines[0]
    );
    let rows: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("{\"row\":"))
        .collect();
    assert_eq!(rows.len(), 2, "resume re-streams the pinned rows: {rows:?}");
    let joined = rows.iter().map(|s| s.as_str()).collect::<String>();
    assert!(
        joined.contains("alice") && joined.contains("bob"),
        "resumed rows round-trip the pinned query: {rows:?}"
    );
    assert!(
        lines.last().unwrap().contains("\"row_count\":2"),
        "resume terminal frame: {}",
        lines.last().unwrap()
    );
}

#[test]
fn resume_from_a_different_tenant_is_masked_as_not_found() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM users")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (_head, lines) = stream_lines(&raw);
    let token = cursor_token(&lines);

    // A different tenant presents the very same token.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_EVIL, PRINCIPAL_ALICE],
    );
    let (head, body) = split_head_body(&raw);
    assert!(
        head.starts_with("HTTP/1.1 404"),
        "cross-tenant resume must be refused with 404, head=\n{head}"
    );
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "refusal must be a non-streaming response, head=\n{head}"
    );
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"code\":\"cursor_not_found\""),
        "must carry the uniform not-found code, body={body}"
    );
    // Existence must not leak: the message must not echo the token or
    // confirm a real cursor was hidden behind an authz wall.
    assert!(
        !body.contains(&token) && !body.to_ascii_lowercase().contains("forbidden"),
        "refusal must not confirm the cursor's existence, body={body}"
    );

    // Sanity — still live for the rightful owner.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, _lines) = stream_lines(&raw);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "owner can still resume after a foreign probe, head=\n{head}"
    );
}

#[test]
fn resume_from_a_different_principal_is_masked_as_not_found() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM users")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (_head, lines) = stream_lines(&raw);
    let token = cursor_token(&lines);

    // Same tenant, different principal.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_MALLORY],
    );
    let (head, body) = split_head_body(&raw);
    assert!(
        head.starts_with("HTTP/1.1 404"),
        "cross-principal resume must be refused with 404, head=\n{head}"
    );
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"code\":\"cursor_not_found\""),
        "must carry the uniform not-found code, body={body}"
    );
}

#[test]
fn resume_after_ttl_is_refused_with_cursor_expired() {
    let h = start_server();
    seed_users(h.addr);

    // Shrink the snapshot TTL so the cursor ages out within the test.
    let (s, b) = put_config(h.addr, "stream.snapshot.ttl_ms", "1");
    assert_eq!(s, 200, "set ttl config: {b}");

    let raw = post_stream(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM users")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (_head, lines) = stream_lines(&raw);
    let token = cursor_token(&lines);

    // Let the 1ms TTL elapse.
    thread::sleep(Duration::from_millis(20));

    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    let (head, body) = split_head_body(&raw);
    assert!(
        head.starts_with("HTTP/1.1 410"),
        "expired resume must be refused with 410 to its owner, head=\n{head}"
    );
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"code\":\"cursor_expired\""),
        "owner must learn the cursor expired, body={body}"
    );
}
