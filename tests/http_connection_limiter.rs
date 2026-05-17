//! Integration test for issue #570 slice 1: clear-text HTTP accept
//! loop must reject connections beyond the limiter cap with
//! `HTTP/1.1 503 Service Unavailable` + `Retry-After`, then recover
//! once existing connections drain.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

fn drain_status_line(stream: &mut TcpStream) -> String {
    // Read up to the end of the first line. The 503 response is
    // small (~80 bytes); use a generous read.
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    text
}

fn boot(cap: usize) -> (String, RedDBServer) {
    let opts = RedDBOptions::in_memory();
    let runtime = RedDBRuntime::with_options(opts).expect("runtime");
    let server = RedDBServer::new(runtime).with_http_limiter_cap(cap);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    // Small wait so the accept thread is parked on `incoming()`.
    thread::sleep(Duration::from_millis(80));
    (addr, server)
}

#[test]
fn rejects_with_503_when_cap_saturated_then_recovers() {
    let cap = 2;
    let (addr, server) = boot(cap);

    // Saturate the cap with `cap` open connections that never send a
    // request. The handler thread is parked on `read_timeout_ms`
    // (5s default) waiting for bytes; that keeps the permit held.
    let mut held: Vec<TcpStream> = Vec::new();
    for _ in 0..cap {
        let s = TcpStream::connect(&addr).expect("connect-hold");
        held.push(s);
    }
    // Give the accept thread a moment to take both permits.
    for _ in 0..50 {
        if server.http_limiter().current() == cap {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        cap,
        "cap should be saturated by held connections"
    );

    // Next connection must be rejected with 503.
    let mut rejected = TcpStream::connect(&addr).expect("connect-rejected");
    rejected
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let text = drain_status_line(&mut rejected);
    assert!(
        text.starts_with("HTTP/1.1 503"),
        "expected 503 status line, got: {text:?}"
    );
    assert!(
        text.contains("Retry-After: 5"),
        "expected Retry-After header, got: {text:?}"
    );
    assert!(
        text.contains("Connection: close"),
        "expected Connection: close, got: {text:?}"
    );

    // Drain the held connections — closing the client side lets the
    // handler thread observe EOF / error and exit, releasing permits.
    for s in held.drain(..) {
        let _ = s.shutdown(std::net::Shutdown::Both);
        drop(s);
    }
    // Wait for permits to drop back down.
    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        0,
        "permits should drain back to zero"
    );

    // Brief settle: the accept loop may still be processing the just-
    // drained sockets when the assertion above sees `current()==0`.
    thread::sleep(Duration::from_millis(100));

    // A fresh request now succeeds (use `/health` — always wired).
    let mut tcp = TcpStream::connect(&addr).expect("connect-recovered");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let resp = String::from_utf8_lossy(&buf);
    // The fresh connection must NOT be the limiter's static 503
    // (signature: `Retry-After: 5` + `Content-Length: 0`). The
    // request itself is allowed to return any status — the point is
    // that it was admitted past the limiter and routed normally.
    let is_limiter_reject = resp.contains("Retry-After: 5") && resp.contains("Content-Length: 0");
    assert!(
        !is_limiter_reject,
        "fresh connection should not be rejected by the limiter, got: {resp:?}"
    );
}
