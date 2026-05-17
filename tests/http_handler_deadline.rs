//! Integration test for issue #570 slice 2: per-handler total-time
//! deadline. A clear-text HTTP handler whose work exceeds the deadline
//! must emit a best-effort `503 Service Unavailable` with
//! `Connection: close`, release its limiter permit on thread exit,
//! and let subsequent requests succeed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

fn boot(handler_timeout: Duration) -> (String, RedDBServer) {
    let opts = RedDBOptions::in_memory();
    let runtime = RedDBRuntime::with_options(opts).expect("runtime");
    let server = RedDBServer::new(runtime).with_handler_timeout(handler_timeout);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    (addr, server)
}

fn send_health(addr: &str, read_timeout: Duration) -> (String, Instant) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(read_timeout)).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    tcp.write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let done = Instant::now();
    (String::from_utf8_lossy(&buf).into_owned(), done)
}

#[test]
fn handler_deadline_emits_503_then_recovers() {
    let handler_timeout = Duration::from_millis(200);
    let inject_ms: u64 = 500;
    let slack = Duration::from_millis(1_500);

    let (addr, server) = boot(handler_timeout);

    // Arm a slow downstream that exceeds the deadline.
    server.set_test_slow_inject_ms(inject_ms);

    let start = Instant::now();
    let (resp, done) = send_health(&addr, Duration::from_secs(5));
    let elapsed = done.duration_since(start);

    // Status line is 503 with Connection: close.
    assert!(
        resp.starts_with("HTTP/1.1 503"),
        "expected 503 status line, got: {resp:?}"
    );
    assert!(
        resp.contains("Connection: close"),
        "expected Connection: close, got: {resp:?}"
    );
    // The deadline-503 has no Retry-After (that signature belongs to
    // the limiter's static reject). This keeps the two failure modes
    // distinguishable in logs and tests.
    assert!(
        !resp.contains("Retry-After:"),
        "deadline-503 should not carry Retry-After, got: {resp:?}"
    );

    // Handler thread must exit within handler_timeout + slack of when
    // it started. The sleep itself is `inject_ms`, so wall-clock is
    // bounded above by `inject_ms + parse + slack`.
    assert!(
        elapsed <= Duration::from_millis(inject_ms) + slack,
        "handler exit too slow: {elapsed:?}"
    );

    // Permit released on thread exit.
    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        0,
        "permit should drop when handler exits"
    );

    // Disarm the slow downstream and verify a subsequent request
    // succeeds — i.e., is admitted, dispatched, and returns a normal
    // response, not the timeout 503.
    server.set_test_slow_inject_ms(0);
    let (resp2, _) = send_health(&addr, Duration::from_secs(5));
    assert!(
        resp2.starts_with("HTTP/1.1 2"),
        "subsequent request should succeed, got: {resp2:?}"
    );
}

#[test]
fn fast_request_unaffected_by_deadline() {
    // With a generous deadline and no injection, /health round-trips
    // normally — the boundary checks must not interfere.
    let (addr, _server) = boot(Duration::from_secs(30));
    let (resp, _) = send_health(&addr, Duration::from_secs(5));
    assert!(
        resp.starts_with("HTTP/1.1 2"),
        "fast request should succeed, got: {resp:?}"
    );
}
