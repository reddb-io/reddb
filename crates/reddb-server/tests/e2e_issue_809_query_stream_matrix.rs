//! Issue #809 / PRD #750 (750e) — streaming-read full contract matrix.
//!
//! Fifth and final slice of the #750 split. The route (#805 / 750a), the
//! bounded-memory executor channel (#806 / 750b), the resumable cursor
//! (#807 / 750c), and cancellation + disconnect (#808 / 750d) each have a
//! focused contract suite. This file is the consolidated end-to-end matrix
//! that exercises the whole streaming family over real HTTP through
//! `RedDBServer`, closing #750's original acceptance criteria 5–8:
//!
//!   * paging — a large scan streams every row exactly once across chunks;
//!   * streaming chunk order — descriptor, then cursor, then rows, then end;
//!   * descriptor-first emission — the descriptor precedes any row;
//!   * cursor expiry / invalidation — TTL aged-out resume is 410, a
//!     cancelled cursor's resume is 409;
//!   * authorization scope — a foreign tenant/principal is masked 404;
//!   * disconnect / cancellation — explicit cancel tombstones, and a client
//!     disconnect tombstones the cursor;
//!   * a representative long-running read — a big result drained through a
//!     deliberately slow reader arrives complete and in order;
//!   * the read-only gate — a mutation is refused without streaming.
//!
//! The normative contract these tests pin is documented in
//! `docs/api/query-streaming.md`.

use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

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

/// Raw request. `extra_headers` are appended verbatim (used for
/// `x-reddb-tenant` and `Authorization`). `read_chunk_size` throttles the
/// reader to exercise backpressure — `None` reads as fast as the OS allows.
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

/// Buffered request returning `(status, body)`.
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

fn post_query(addr: std::net::SocketAddr, sql: &str) -> (u16, String) {
    let body = format!("{{\"query\": {}}}", json_string(sql));
    request_collect(addr, "POST", "/query", &[], &body)
}

fn put_config(addr: std::net::SocketAddr, key: &str, value_json: &str) -> (u16, String) {
    let body = format!("{{\"value\": {value_json}}}");
    request_collect(addr, "PUT", &format!("/config/{key}"), &[], &body)
}

/// POST /query/stream with optional tenant/principal headers and a raw JSON
/// body (used both for fresh opens and `{"cursor":...}` resumes).
fn post_stream(
    addr: std::net::SocketAddr,
    body: &str,
    extra_headers: &[(&str, &str)],
    read_chunk_size: Option<usize>,
    inter_read_delay: Option<Duration>,
) -> Vec<u8> {
    request_raw(
        addr,
        "POST",
        "/query/stream",
        extra_headers,
        body,
        read_chunk_size,
        inter_read_delay,
    )
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

/// Decode an HTTP/1.1 `Transfer-Encoding: chunked` body into
/// `(payload, chunk_count)`. The chunk count reflects how many times the
/// server flushed.
fn dechunk(body: &[u8]) -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    let mut chunks = 0usize;
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
        chunks += 1;
        i += size + 2;
    }
    (payload, chunks)
}

/// Decode the chunked NDJSON body into `(head, trimmed frame lines, chunks)`.
fn stream_lines(raw: &[u8]) -> (String, Vec<String>, usize) {
    let (head, body) = split_head_body(raw);
    let (payload, chunks) = dechunk(&body);
    let text = String::from_utf8(payload).expect("utf8 ndjson");
    let lines = text
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (head, lines, chunks)
}

/// Lift the opaque cursor token out of the `{"cursor":{...}}` control frame.
fn cursor_token(lines: &[String]) -> String {
    let frame = lines
        .iter()
        .find(|l| l.starts_with("{\"cursor\":"))
        .expect("a cursor control frame must be present");
    token_after_needle(frame).expect("cursor frame carries a token")
}

/// Lift the 48-hex token out of any text containing `"token":"…"`. Works on
/// partial raw bytes too, so a disconnect test can grab the token from the
/// prelude without fully draining the stream.
fn token_after_needle(text: &str) -> Option<String> {
    let needle = "\"token\":\"";
    let start = text.find(needle)? + needle.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

const TENANT_ACME: (&str, &str) = ("x-reddb-tenant", "acme");
const TENANT_EVIL: (&str, &str) = ("x-reddb-tenant", "evil-corp");
const PRINCIPAL_ALICE: (&str, &str) = ("Authorization", "Bearer alice-token");
const PRINCIPAL_MALLORY: (&str, &str) = ("Authorization", "Bearer mallory-token");

fn seed_users(addr: std::net::SocketAddr) {
    let (s, b) = post_query(addr, "CREATE TABLE users (id INT, name TEXT)");
    assert_eq!(s, 200, "create table: {b}");
    let (s, b) = post_query(
        addr,
        "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_eq!(s, 200, "insert: {b}");
}

/// Seed a single-column table with `n` rows via batched inserts, so a scan
/// over it is large enough to span multiple wire chunks.
fn seed_big(addr: std::net::SocketAddr, n: usize) {
    let (s, b) = post_query(addr, "CREATE TABLE big (id INT)");
    assert_eq!(s, 200, "create big: {b}");
    let mut i = 0usize;
    while i < n {
        let end = (i + 1000).min(n);
        let mut values = String::new();
        for j in i..end {
            if j > i {
                values.push_str(", ");
            }
            values.push_str(&format!("({j})"));
        }
        let (s, b) = post_query(addr, &format!("INSERT INTO big (id) VALUES {values}"));
        assert_eq!(s, 200, "bulk insert [{i}..{end}): {b}");
        i = end;
    }
}

/// Open a fresh stream as acme/alice and return its cursor token.
fn open_cursor(addr: std::net::SocketAddr) -> String {
    let raw = post_stream(
        addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("SELECT id, name FROM users")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, lines, _chunks) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "open head=\n{head}");
    cursor_token(&lines)
}

#[test]
fn chunk_order_is_descriptor_then_cursor_then_rows_then_end() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream(
        h.addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("SELECT id, name FROM users")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, lines, _chunks) = stream_lines(&raw);

    assert!(head.starts_with("HTTP/1.1 200"), "head=\n{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "stream must use chunked encoding, head=\n{head}"
    );
    assert!(
        head.to_ascii_lowercase().contains("application/x-ndjson"),
        "stream must be NDJSON, head=\n{head}"
    );

    // Frame 1 — descriptor, before any row, with columns + fingerprint.
    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "descriptor must be first: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\"schema_fingerprint\"")
            && lines[0].contains("\"id\"")
            && lines[0].contains("\"name\""),
        "descriptor carries fingerprint + columns: {}",
        lines[0]
    );
    // Frame 2 — cursor control frame with token + snapshot pin + TTL.
    assert!(
        lines[1].starts_with("{\"cursor\":"),
        "cursor frame follows the descriptor: {}",
        lines[1]
    );
    assert!(
        lines[1].contains("\"token\":\"")
            && lines[1].contains("\"snapshot_lsn\":")
            && lines[1].contains("\"ttl_ms\":")
            && lines[1].contains("\"expires_at_ms\":"),
        "cursor frame carries token + snapshot pin + TTL: {}",
        lines[1]
    );
    // Rows in between, then the terminal end frame last.
    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(rows, 2, "two row frames: {lines:?}");
    let end = lines.last().unwrap();
    assert!(
        end.starts_with("{\"end\":") && end.contains("\"row_count\":2"),
        "terminal end frame with row_count=2: {end}"
    );
}

#[test]
fn paging_streams_every_row_once_across_multiple_chunks() {
    let h = start_server();
    // > chunk_max_rows (default 1000) so the producer flushes incrementally
    // and the body spans more than one wire chunk.
    const N: usize = 1500;
    seed_big(h.addr, N);

    let raw = post_stream(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM big")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, lines, chunks) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "head=\n{head}");
    assert!(
        chunks >= 2,
        "a large scan must flush incrementally (bounded buffer), saw {chunks} chunk(s)"
    );

    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(rows, N, "every row streams exactly once: got {rows}");
    assert!(
        lines
            .last()
            .unwrap()
            .contains(&format!("\"row_count\":{N}")),
        "terminal frame reports the full count: {}",
        lines.last().unwrap()
    );
}

#[test]
fn representative_long_running_read_drains_complete_under_a_slow_reader() {
    let h = start_server();
    const N: usize = 2000;
    seed_big(h.addr, N);

    // Deliberately slow reader: 256-byte reads with a small inter-read
    // delay. The descriptor must still arrive first, every row must be
    // delivered in order with no loss, and the body must span chunks.
    let raw = post_stream(
        h.addr,
        &format!("{{\"query\": {}}}", json_string("SELECT id FROM big")),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        Some(256),
        Some(Duration::from_millis(1)),
    );
    let (head, lines, chunks) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "head=\n{head}");
    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "descriptor still first under a slow reader: {}",
        lines[0]
    );
    assert!(
        chunks >= 2,
        "slow read still flushes incrementally: {chunks}"
    );

    // Rows arrive in ascending id order (scan order) with none missing.
    let ids: Vec<usize> = lines
        .iter()
        .filter(|l| l.starts_with("{\"row\":"))
        .map(|l| {
            let needle = "\"id\":";
            let start = l.find(needle).expect("row id") + needle.len();
            let rest = &l[start..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest[..end].parse::<usize>().expect("numeric id")
        })
        .collect();
    assert_eq!(ids.len(), N, "slow reader receives every row");
    assert_eq!(
        ids,
        (0..N).collect::<Vec<_>>(),
        "rows arrive in scan order with no loss or reorder"
    );
}

#[test]
fn read_only_gate_refuses_mutation_without_streaming() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream(
        h.addr,
        &format!(
            "{{\"query\": {}}}",
            json_string("INSERT INTO users (id, name) VALUES (3, 'carol')")
        ),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, body) = split_head_body(&raw);
    assert!(
        head.starts_with("HTTP/1.1 400"),
        "mutation refused with 400, head=\n{head}"
    );
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "refusal is non-streaming, head=\n{head}"
    );
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"code\":\"stream_unsupported_statement\"")
            && body.contains("\"statement_kind\":\"mutation\""),
        "structured refusal names the statement kind: {body}"
    );

    // The refused mutation must not have run.
    let (_s, after) = post_query(h.addr, "SELECT id FROM users");
    assert!(
        !after.contains("carol") && !after.contains("\"id\":3"),
        "refused mutation did not execute: {after}"
    );
}

#[test]
fn cursor_resume_restreams_the_pinned_view() {
    let h = start_server();
    seed_users(h.addr);
    let token = open_cursor(h.addr);

    // Resume with ONLY the cursor — no query — same tenant + principal.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, lines, _chunks) = stream_lines(&raw);
    assert!(head.starts_with("HTTP/1.1 200"), "resume head=\n{head}");
    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "resume replays a descriptor-first stream: {}",
        lines[0]
    );
    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(rows, 2, "resume re-streams the pinned rows: {lines:?}");
}

#[test]
fn authorization_scope_masks_foreign_tenant_and_principal_as_404() {
    let h = start_server();
    seed_users(h.addr);
    let token = open_cursor(h.addr);

    // Foreign tenant — masked 404, no existence leak.
    let (status, body) = request_collect(
        h.addr,
        "POST",
        "/query/stream",
        &[TENANT_EVIL, PRINCIPAL_ALICE],
        &format!("{{\"cursor\": {}}}", json_string(&token)),
    );
    assert_eq!(status, 404, "cross-tenant resume masked: {body}");
    assert!(body.contains("cursor_not_found"), "uniform code: {body}");
    assert!(
        !body.contains(&token) && !body.to_ascii_lowercase().contains("forbidden"),
        "no existence leak: {body}"
    );

    // Foreign principal (same tenant) — also masked 404.
    let (status, body) = request_collect(
        h.addr,
        "POST",
        "/query/stream",
        &[TENANT_ACME, PRINCIPAL_MALLORY],
        &format!("{{\"cursor\": {}}}", json_string(&token)),
    );
    assert_eq!(status, 404, "cross-principal resume masked: {body}");
    assert!(body.contains("cursor_not_found"), "uniform code: {body}");

    // The rightful owner is unaffected by the foreign probes.
    let raw = post_stream(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
        None,
        None,
    );
    let (head, _lines, _chunks) = stream_lines(&raw);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "owner still resumes after foreign probes: {head}"
    );
}

#[test]
fn cursor_expiry_after_ttl_is_refused_410_to_owner() {
    let h = start_server();
    seed_users(h.addr);

    // Shrink the snapshot TTL so the cursor ages out within the test.
    let (s, b) = put_config(h.addr, "stream.snapshot.ttl_ms", "1");
    assert_eq!(s, 200, "set ttl config: {b}");

    let token = open_cursor(h.addr);
    thread::sleep(Duration::from_millis(20));

    let (status, body) = request_collect(
        h.addr,
        "POST",
        "/query/stream",
        &[TENANT_ACME, PRINCIPAL_ALICE],
        &format!("{{\"cursor\": {}}}", json_string(&token)),
    );
    assert_eq!(status, 410, "expired resume refused 410: {body}");
    assert!(
        body.contains("cursor_expired"),
        "owner learns expiry: {body}"
    );
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

    // Cancel is idempotent.
    let (status, body) = post_cancel(
        h.addr,
        &format!("{{\"cursor\": {}}}", json_string(&token)),
        &[TENANT_ACME, PRINCIPAL_ALICE],
    );
    assert_eq!(status, 200, "second cancel still 200: {body}");

    // Resuming a tombstoned cursor is refused with a dedicated reason,
    // distinct from expiry (410) and unknown/foreign (404).
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
        "dedicated reason: {body}"
    );
}

#[test]
fn client_disconnect_midstream_tombstones_the_cursor() {
    let h = start_server();
    // A large result guarantees the server is still producing rows (blocked
    // on a full socket buffer) when the client goes away, so the disconnect
    // is observed as a broken-pipe write failure.
    seed_big(h.addr, 20_000);

    // Open a raw stream, read just enough of the prelude to lift the cursor
    // token, then drop the connection mid-stream.
    let body = format!("{{\"query\": {}}}", json_string("SELECT id FROM big"));
    let request = format!(
        "POST /query/stream HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{}: {}\r\n{}: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        TENANT_ACME.0,
        TENANT_ACME.1,
        PRINCIPAL_ALICE.0,
        PRINCIPAL_ALICE.1,
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(h.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");

    // Read a small prefix until the cursor token appears (or a cap is hit).
    let mut prefix = Vec::new();
    let mut buf = [0u8; 512];
    let token = loop {
        let n = stream.read(&mut buf).expect("read prelude");
        if n == 0 {
            panic!("stream closed before cursor frame");
        }
        prefix.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&prefix);
        if let Some(tok) = token_after_needle(&text) {
            break tok;
        }
        assert!(prefix.len() < 64 * 1024, "cursor frame not seen in prelude");
    };

    // Drop the connection mid-stream — the server's next write fails with a
    // broken pipe, which raises the cancel token and tombstones the cursor.
    stream.shutdown(Shutdown::Both).ok();
    drop(stream);

    // The tombstone is applied when the server next attempts a write, so
    // poll the resume path under a bounded deadline until it is refused.
    let deadline = Instant::now() + Duration::from_secs(10);
    let (status, resume_body) = loop {
        let result = request_collect(
            h.addr,
            "POST",
            "/query/stream",
            &[TENANT_ACME, PRINCIPAL_ALICE],
            &format!("{{\"cursor\": {}}}", json_string(&token)),
        );
        if result.0 == 409 || Instant::now() >= deadline {
            break result;
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        status, 409,
        "disconnect must tombstone the cursor (resume → 409): {resume_body}"
    );
    assert!(
        resume_body.contains("cursor_cancelled"),
        "disconnect tombstone surfaces as cursor_cancelled: {resume_body}"
    );
}
