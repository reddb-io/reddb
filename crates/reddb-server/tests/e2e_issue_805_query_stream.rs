//! Issue #805 / PRD #750 — `/query/stream` NDJSON streaming transport.
//!
//! First tracer-bullet of the #750 split: a dedicated streaming-read
//! transport for read-only SELECT queries. These tests pin the wire
//! contract end-to-end through `RedDBServer` over real HTTP:
//!
//!   1. A representative SELECT streams NDJSON: a descriptor frame
//!      FIRST (columns + types + schema_fingerprint), then one `row`
//!      frame per record, then an `end` frame — over chunked encoding.
//!   2. A non-read-only statement (INSERT) is refused with a structured
//!      error naming the statement kind, with NO streamed body.
//!   3. Backpressure: a deliberately slow reader still receives the
//!      complete, correctly-ordered stream, and the body is delivered
//!      in multiple HTTP chunks (the writer flushes incrementally at the
//!      row cap rather than buffering the whole result).
//!   4. Existing `/query` behaviour is untouched: a plain POST /query
//!      still returns a single buffered JSON envelope.

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

/// Buffered POST /query (the legacy path) — returns `(status, body)`.
fn post_query(addr: std::net::SocketAddr, sql: &str) -> (u16, String) {
    let body = format!("{{\"query\": {}}}", json_string(sql));
    let request = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let (head, body) = response.split_once("\r\n\r\n").expect("http framing");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("parse status");
    (status, body.to_string())
}

/// Raw POST to `/query/stream`. Returns the full raw HTTP response
/// bytes (head + chunked body). `read_chunk_size` throttles the reader
/// to exercise backpressure — `None` reads as fast as the OS allows.
fn post_stream_raw(
    addr: std::net::SocketAddr,
    sql: &str,
    read_chunk_size: Option<usize>,
    inter_read_delay: Option<Duration>,
) -> Vec<u8> {
    let body = format!("{{\"query\": {}}}", json_string(sql));
    let request = format!(
        "POST /query/stream HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

/// Split a raw HTTP response into `(head, body_bytes)`.
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
/// `(payload, chunk_count)`. The chunk count is the number of non-empty
/// wire chunks — it reflects how many times the server flushed.
fn dechunk(body: &[u8]) -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    let mut chunks = 0usize;
    let mut i = 0usize;
    loop {
        // chunk size line: <hex>\r\n
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
        i += size + 2; // skip data + trailing \r\n
    }
    (payload, chunks)
}

/// Decode the chunked NDJSON body into one trimmed line per frame.
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

fn seed_users(addr: std::net::SocketAddr) {
    let (s, b) = post_query(addr, "CREATE TABLE users (id INT, name TEXT)");
    assert_eq!(s, 200, "create table: {b}");
    let (s, b) = post_query(
        addr,
        "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_eq!(s, 200, "insert: {b}");
}

#[test]
fn query_stream_emits_descriptor_first_then_rows_then_end() {
    let h = start_server();
    seed_users(h.addr);

    let raw = post_stream_raw(h.addr, "SELECT id, name FROM users", None, None);
    let (head, lines, _chunks) = stream_lines(&raw);

    assert!(
        head.starts_with("HTTP/1.1 200"),
        "expected 200 streaming response, head=\n{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "stream must use chunked encoding, head=\n{head}"
    );
    assert!(
        head.to_ascii_lowercase().contains("application/x-ndjson"),
        "stream must be NDJSON, head=\n{head}"
    );

    assert!(
        lines.len() >= 4,
        "expected descriptor + 2 rows + end, got {} lines: {lines:?}",
        lines.len()
    );

    // Frame 1 — descriptor, BEFORE any row.
    let descriptor = &lines[0];
    assert!(
        descriptor.starts_with("{\"descriptor\":"),
        "first frame must be the descriptor, got: {descriptor}"
    );
    assert!(
        descriptor.contains("\"schema_fingerprint\""),
        "descriptor must carry a schema_fingerprint, got: {descriptor}"
    );
    assert!(
        descriptor.contains("\"columns\"")
            && descriptor.contains("\"id\"")
            && descriptor.contains("\"name\""),
        "descriptor must name the columns for UI init, got: {descriptor}"
    );

    // Frames 2..N-1 — rows.
    let rows: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("{\"row\":"))
        .collect();
    assert_eq!(rows.len(), 2, "expected 2 row frames, got: {rows:?}");
    let joined = rows.iter().map(|s| s.as_str()).collect::<String>();
    assert!(
        joined.contains("alice") && joined.contains("bob"),
        "row payloads should round-trip, rows={rows:?}"
    );

    // Final frame — end with row_count.
    let end = lines.last().unwrap();
    assert!(
        end.starts_with("{\"end\":") && end.contains("\"row_count\":2"),
        "last frame must be the terminal end envelope with row_count=2, got: {end}"
    );
}

#[test]
fn query_stream_refuses_non_read_only_with_named_kind() {
    let h = start_server();
    seed_users(h.addr);

    // An INSERT is a mutation — must be refused with a non-streaming,
    // structured error naming the statement kind.
    let raw = post_stream_raw(
        h.addr,
        "INSERT INTO users (id, name) VALUES (3, 'carol')",
        None,
        None,
    );
    let (head, body) = split_head_body(&raw);

    assert!(
        head.starts_with("HTTP/1.1 400"),
        "mutation must be refused with 400, head=\n{head}"
    );
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "refusal must be a non-streaming response, head=\n{head}"
    );
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"code\":\"stream_unsupported_statement\""),
        "refusal must carry the structured code, body={body}"
    );
    assert!(
        body.contains("\"statement_kind\":\"mutation\""),
        "refusal must name the rejected statement kind, body={body}"
    );

    // The refused INSERT must NOT have run.
    let (_s, after) = post_query(h.addr, "SELECT id FROM users");
    assert!(
        !after.contains("carol") && !after.contains("\"id\":3"),
        "refused mutation must not have been executed, after={after}"
    );
}

#[test]
fn query_stream_backpressure_slow_reader_gets_full_stream_in_multiple_chunks() {
    let h = start_server();
    let (s, b) = post_query(h.addr, "CREATE TABLE big (id INT)");
    assert_eq!(s, 200, "create: {b}");
    // > chunk_max_rows (default 1000) guarantees the producer flushes at
    // the row cap, so the body is delivered in multiple chunks rather
    // than buffered whole — the bounded-buffer backpressure property.
    let mut values = String::new();
    for i in 0..1500 {
        if i > 0 {
            values.push_str(", ");
        }
        values.push_str(&format!("({i})"));
    }
    let (s, b) = post_query(h.addr, &format!("INSERT INTO big (id) VALUES {values}"));
    assert_eq!(s, 200, "bulk insert: {b}");

    // Deliberately slow reader: 256-byte reads with a small delay.
    let raw = post_stream_raw(
        h.addr,
        "SELECT id FROM big",
        Some(256),
        Some(Duration::from_millis(1)),
    );
    let (head, lines, chunks) = stream_lines(&raw);

    assert!(head.starts_with("HTTP/1.1 200"), "head=\n{head}");
    assert!(
        chunks >= 2,
        "writer must flush incrementally (bounded buffer), saw {chunks} chunk(s)"
    );

    assert!(
        lines[0].starts_with("{\"descriptor\":"),
        "descriptor must still arrive first under a slow reader: {}",
        lines[0]
    );
    let rows = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
    assert_eq!(
        rows, 1500,
        "slow reader must receive every row without loss"
    );
    let end = lines.last().unwrap();
    assert!(
        end.contains("\"row_count\":1500"),
        "terminal frame must report the full row count, got: {end}"
    );
}

#[test]
fn legacy_query_endpoint_is_untouched() {
    let h = start_server();
    seed_users(h.addr);

    // A plain POST /query must still return a single buffered JSON
    // envelope (Content-Length, not chunked NDJSON frames).
    let (status, body) = post_query(h.addr, "SELECT id, name FROM users");
    assert_eq!(status, 200, "body={body}");
    assert!(
        !body.contains("{\"descriptor\":") && !body.contains("{\"row\":"),
        "legacy /query must not emit streaming frames, body={body}"
    );
    assert!(
        body.contains("\"records\"") || body.contains("\"result\""),
        "legacy /query must keep its buffered envelope shape, body={body}"
    );
}
