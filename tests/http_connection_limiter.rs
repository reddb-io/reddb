//! Integration test for issue #570 slice 1: clear-text HTTP accept
//! loop must reject connections beyond the limiter cap with
//! `HTTP/1.1 503 Service Unavailable` + `Retry-After`, then recover
//! once existing connections drain.

#[allow(dead_code)]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;

fn send_request(addr: &str, path: &str) -> String {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn spawn_slow_request(addr: String) -> thread::JoinHandle<String> {
    thread::spawn(move || send_request(&addr, "/health/live"))
}

fn boot(cap: usize) -> (support::TempDbFile, String, RedDBServer) {
    let (db, runtime) = support::persistent_runtime("http-connection-limiter");
    let server = RedDBServer::new(runtime).with_http_limiter_cap(cap);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    // Small wait so the accept thread is parked on `incoming()`.
    thread::sleep(Duration::from_millis(80));
    (db, addr, server)
}

#[test]
fn rejects_with_503_when_cap_saturated_then_recovers() {
    let cap = 2;
    let (_db, addr, server) = boot(cap);

    // The async HTTP edge limits in-flight requests, not idle TCP
    // connections. Saturate the cap with real requests held inside the
    // test slow-inject hook.
    server.set_test_slow_inject_ms(2_000);
    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(spawn_slow_request(addr.clone()));
    }
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
    let text = send_request(&addr, "/health/live");
    assert!(
        text.starts_with("HTTP/1.1 503"),
        "expected 503 status line, got: {text:?}"
    );
    let text_lower = text.to_ascii_lowercase();
    assert!(
        text_lower.contains("retry-after: 5"),
        "expected Retry-After header, got: {text:?}"
    );

    // Drain the held requests; their handlers finish once the
    // slow-inject sleep expires, releasing permits.
    server.set_test_slow_inject_ms(0);
    for handle in held {
        let body = handle.join().expect("held request thread");
        assert!(
            body.starts_with("HTTP/1.1 2"),
            "held request should complete normally, got: {body:?}"
        );
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
    // The fresh connection must NOT be the limiter's capacity 503.
    // The request itself is allowed to return any status — the point is
    // that it was admitted past the limiter and routed normally.
    let resp_lower = resp.to_ascii_lowercase();
    let is_limiter_reject = resp.starts_with("HTTP/1.1 503")
        && resp_lower.contains("retry-after: 5")
        && resp.contains("server at capacity");
    assert!(
        !is_limiter_reject,
        "fresh connection should not be rejected by the limiter, got: {resp:?}"
    );
}
