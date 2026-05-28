//! Issue #730 / slice E of PRD #718 — HTTP smoke for
//! `QUEUE READ … WAIT <duration>`.
//!
//! The runtime path (#728) and the cap/txn rejection path (#727) are
//! already pinned by their own runtime-level tests. This file exists
//! only to verify that the four canonical WAIT outcomes survive a
//! real HTTP round-trip through `RedDBServer`:
//!
//!   1. Empty queue + WAIT 200ms → HTTP 200 with empty `records`,
//!      and the round-trip blocks for ~the budget.
//!   2. Producer enqueues from a separate HTTP client during the
//!      waiter's WAIT → the message is delivered over HTTP.
//!   3. WAIT > server cap → HTTP 400 whose body names the cap key
//!      (`red.config.queue.max_wait_ms`) and the active cap value.
//!   4. Registry cancellation while a WAIT is parked → HTTP 400 with
//!      the explicit `QUEUE READ WAIT cancelled` message — not a
//!      200/empty timeout. Today the only cancellation primitive on
//!      the registry is `cancel_all()` (the disconnect-driven
//!      per-waiter cancel hinted at by the brief is not wired into
//!      this server, which does not use axum); the smoke pins the
//!      HTTP propagation path the same way the #728 runtime test
//!      pins the runtime path.

use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct ServerHandle {
    addr: std::net::SocketAddr,
    server: RedDBServer,
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
        server,
        _join: join,
    }
}

/// Issue a single POST /query, read the full response (Connection:
/// close so the server flushes + drops on its own), and return
/// `(status, body)`.
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
    parse_http_response(&response)
}

fn parse_http_response(raw: &str) -> (u16, String) {
    let (head, body) = raw.split_once("\r\n\r\n").expect("http framing");
    let status_line = head.lines().next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("parse status");
    (status, body.to_string())
}

/// Minimal JSON string escaper for the small set of SQL bodies used
/// in this file. The smoke tests do not pass arbitrary user input.
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

fn records_count(body: &str) -> usize {
    // The query envelope is `{"result":{"columns":[...],"records":[...]}}`.
    // For smoke-level checks, counting message_id occurrences in the
    // records segment is enough to distinguish empty vs delivered
    // without pulling a JSON parser into a transport-shape test.
    let start = match body.find("\"records\":[") {
        Some(i) => i + "\"records\":[".len(),
        None => return 0,
    };
    // Find matching ']' at depth 0 considering nested brackets in values.
    let bytes = body.as_bytes();
    let mut depth = 1usize;
    let mut end = bytes.len();
    for (idx, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + idx;
                    break;
                }
            }
            _ => {}
        }
    }
    let slice = &body[start..end];
    slice.matches("\"message_id\"").count()
}

#[test]
fn http_wait_returns_empty_after_budget_when_queue_stays_empty() {
    let h = start_server();
    let (s, _) = post_query(h.addr, "CREATE QUEUE qhttp_empty");
    assert_eq!(s, 200);
    let (s, _) = post_query(h.addr, "QUEUE GROUP CREATE qhttp_empty workers");
    assert_eq!(s, 200);

    let started = Instant::now();
    let (status, body) = post_query(
        h.addr,
        "QUEUE READ qhttp_empty GROUP workers CONSUMER c1 COUNT 1 WAIT 800ms",
    );
    let elapsed = started.elapsed();
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        records_count(&body),
        0,
        "WAIT timeout should deliver an empty projection over HTTP, body={body}"
    );
    assert!(
        elapsed >= Duration::from_millis(750),
        "round-trip should park at least ~the WAIT budget, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "round-trip should not stall past the budget, elapsed={elapsed:?}"
    );
}

#[test]
fn http_enqueue_during_wait_delivers_message_to_waiter() {
    let h = start_server();
    let (s, _) = post_query(h.addr, "CREATE QUEUE qhttp_wake");
    assert_eq!(s, 200);
    let (s, _) = post_query(h.addr, "QUEUE GROUP CREATE qhttp_wake workers");
    assert_eq!(s, 200);

    let producer_addr = h.addr;
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        post_query(producer_addr, "QUEUE PUSH qhttp_wake 'wakeup'")
    });

    let started = Instant::now();
    let (status, body) = post_query(
        h.addr,
        "QUEUE READ qhttp_wake GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
    );
    let elapsed = started.elapsed();
    let (push_status, _) = producer.join().expect("producer join");

    assert_eq!(push_status, 200);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        records_count(&body),
        1,
        "committed enqueue must wake the waiter and deliver, body={body}"
    );
    assert!(
        body.contains("wakeup"),
        "delivered payload should round-trip in the response body, body={body}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "post-commit notify should wake well before the 5s budget (elapsed={elapsed:?})"
    );
}

#[test]
fn http_wait_above_cap_is_rejected_with_explicit_error() {
    let h = start_server();
    let (s, _) = post_query(h.addr, "CREATE QUEUE qhttp_cap");
    assert_eq!(s, 200);
    let (s, _) = post_query(h.addr, "QUEUE GROUP CREATE qhttp_cap workers");
    assert_eq!(s, 200);

    let started = Instant::now();
    let (status, body) = post_query(
        h.addr,
        "QUEUE READ qhttp_cap GROUP workers CONSUMER c1 COUNT 1 WAIT 999h",
    );
    let elapsed = started.elapsed();

    assert_eq!(
        status, 400,
        "WAIT > cap should reject with 400, body={body}"
    );
    assert!(
        body.contains("red.config.queue.max_wait_ms"),
        "rejection should name the cap key over HTTP, body={body}"
    );
    assert!(
        body.contains("60000"),
        "rejection should name the active cap value (default 60000ms), body={body}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "cap rejection must not park before refusing, elapsed={elapsed:?}"
    );
}

#[test]
fn http_wait_cancellation_returns_explicit_error_not_empty_timeout() {
    let h = start_server();
    let (s, _) = post_query(h.addr, "CREATE QUEUE qhttp_cancel");
    assert_eq!(s, 200);
    let (s, _) = post_query(h.addr, "QUEUE GROUP CREATE qhttp_cancel workers");
    assert_eq!(s, 200);

    // Trigger cancellation mid-WAIT through the runtime registry. The
    // brief frames cancellation as "client disconnect (axum)" — this
    // server does not use axum and has no per-connection disconnect→
    // cancel wiring today, so the only cancellation primitive on the
    // registry is `cancel_all()`. The HTTP-layer assertion is the
    // load-bearing one: the response must be an explicit cancellation
    // error, not a 200/empty timeout.
    let registry = Arc::clone(&h.server.runtime().queue_wait_registry());
    let canceler = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        registry.cancel_all();
    });

    let (status, body) = post_query(
        h.addr,
        "QUEUE READ qhttp_cancel GROUP workers CONSUMER c1 COUNT 1 WAIT 5s",
    );
    canceler.join().expect("canceler join");

    assert_eq!(
        status, 400,
        "cancellation must surface as an HTTP error, not a 200 empty projection, body={body}"
    );
    assert!(
        body.contains("WAIT cancelled") || body.contains("shutting down"),
        "cancellation error should be explicit over HTTP, body={body}"
    );

    // Leave the shared registry in a known state in case future cases
    // share a process-wide counter (none today, cheap insurance).
    h.server.runtime().queue_wait_registry().reset_cancelled();
}
