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
    send_method(addr, "GET", path)
}

fn send_method(addr: &str, method: &str, path: &str) -> String {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn spawn_slow_request(addr: String) -> thread::JoinHandle<String> {
    thread::spawn(move || send_request(&addr, "/__test/slow-query"))
}

fn boot(cap: usize) -> (support::TempDbFile, String, RedDBServer) {
    boot_with_health_cap(cap, 4)
}

fn boot_with_execution_workers(
    cap: usize,
    execution_workers: usize,
    handler_timeout: Duration,
) -> (support::TempDbFile, String, RedDBServer) {
    let (db, runtime) = support::persistent_runtime("http-execution-admission");
    let server = RedDBServer::new(runtime)
        .with_http_limiter_cap(cap)
        .with_http_execution_workers(execution_workers)
        .with_handler_timeout(handler_timeout);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    (db, addr, server)
}

fn boot_with_health_cap(
    cap: usize,
    health_cap: usize,
) -> (support::TempDbFile, String, RedDBServer) {
    let (db, runtime) = support::persistent_runtime("http-connection-limiter");
    let server = RedDBServer::new(runtime)
        .with_http_limiter_cap(cap)
        .with_health_limiter_cap(health_cap);
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
fn health_reserve_is_exact_bounded_and_independent() {
    let (_db, addr, server) = boot_with_health_cap(1, 1);
    server.set_test_slow_inject_ms(2_000);

    // Fill the one-slot health reserve without consuming normal capacity.
    let held_health_addr = addr.clone();
    let held_health = thread::spawn(move || send_request(&held_health_addr, "/health/live"));
    for _ in 0..50 {
        if server.health_limiter().current() == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.health_limiter().current(), 1);
    assert_eq!(server.http_limiter().current(), 0);

    // The reserve is bounded: a second exact health request is refused.
    let rejected_health = send_request(&addr, "/health/startup");
    assert!(
        rejected_health.starts_with("HTTP/1.1 503"),
        "full health reserve must reject another probe: {rejected_health:?}"
    );

    // Health saturation does not consume the normal query pool.
    let held_query_addr = addr.clone();
    let held_query = thread::spawn(move || send_request(&held_query_addr, "/__test/normal-query"));
    for _ in 0..50 {
        if server.http_limiter().current() == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.http_limiter().current(), 1);
    assert_eq!(server.health_limiter().current(), 1);

    // Prefix lookalikes and non-GET methods do not enter the reserve.
    for (method, path) in [("GET", "/health/live/extra"), ("POST", "/health/live")] {
        let rejected = send_method(&addr, method, path);
        assert!(
            rejected.starts_with("HTTP/1.1 503"),
            "{method} {path} must use the saturated normal limiter: {rejected:?}"
        );
    }

    server.set_test_slow_inject_ms(0);
    let health = held_health.join().expect("held health request");
    assert!(
        health.starts_with("HTTP/1.1 2"),
        "health failed: {health:?}"
    );
    let query = held_query.join().expect("held query request");
    assert!(
        query.starts_with("HTTP/1.1 404"),
        "normal request did not route after admission: {query:?}"
    );
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

    // A normal request must still be rejected with 503.
    let text = send_request(&addr, "/__test/rejected-query");
    assert!(
        text.starts_with("HTTP/1.1 503"),
        "expected 503 status line, got: {text:?}"
    );
    let text_lower = text.to_ascii_lowercase();
    assert!(
        text_lower.contains("retry-after: 5"),
        "expected Retry-After header, got: {text:?}"
    );

    // Kubernetes health probes must retain a small, separately bounded
    // reserve even while normal query capacity is exhausted. Otherwise a
    // busy but healthy process is removed from its EndpointSlice and the
    // overload cascades into a full service outage.
    let health_addr = addr.clone();
    let health_handle = thread::spawn(move || send_request(&health_addr, "/health/ready"));
    for _ in 0..50 {
        if server.health_limiter().current() == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.health_limiter().current(),
        1,
        "health request should occupy only the reserved limiter"
    );
    assert_eq!(
        server.http_limiter().current(),
        cap,
        "health admission must not consume or release normal capacity"
    );
    let health = health_handle.join().expect("health request thread");
    assert!(
        health.starts_with("HTTP/1.1 2"),
        "health must remain available while normal capacity is saturated: {health:?}"
    );

    // Drain the held requests; their handlers finish once the
    // slow-inject sleep expires, releasing permits.
    server.set_test_slow_inject_ms(0);
    for handle in held {
        let body = handle.join().expect("held request thread");
        assert!(
            body.starts_with("HTTP/1.1 404"),
            "held non-health request should complete normally, got: {body:?}"
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
        && resp_lower.contains("server at capacity");
    assert!(
        !is_limiter_reject,
        "fresh connection should not be rejected by the limiter, got: {resp:?}"
    );
}

#[test]
fn queues_admitted_requests_without_exceeding_execution_workers() {
    let cap = 3;
    let (_db, addr, server) = boot_with_execution_workers(cap, 1, Duration::from_secs(30));
    server.set_test_slow_inject_ms(2_000);

    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(spawn_slow_request(addr.clone()));
    }

    for _ in 0..100 {
        if server.http_limiter().current() == cap
            && server.http_execution_active() == 1
            && server.http_execution_queued() == 2
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.http_limiter().current(), cap);
    assert_eq!(server.http_execution_workers(), 1);
    assert_eq!(server.http_execution_active(), 1);
    assert_eq!(server.http_execution_queued(), 2);

    let rejected = send_request(&addr, "/__test/envelope-full");
    assert!(
        rejected.starts_with("HTTP/1.1 503"),
        "the request envelope should reject only after its active and queued slots fill: {rejected:?}"
    );

    // The active request keeps its original sleep, but queued requests read
    // the cleared hook only after receiving the single execution slot.
    server.set_test_slow_inject_ms(0);
    for handle in held {
        let response = handle.join().expect("admitted request thread");
        assert!(
            response.starts_with("HTTP/1.1 404"),
            "admitted request should eventually execute: {response:?}"
        );
    }

    for _ in 0..100 {
        if server.http_limiter().current() == 0
            && server.http_execution_active() == 0
            && server.http_execution_queued() == 0
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.http_limiter().current(), 0);
    assert_eq!(server.http_execution_active(), 0);
    assert_eq!(server.http_execution_queued(), 0);
}

#[test]
fn queue_wait_consumes_the_total_handler_deadline() {
    let (_db, addr, server) = boot_with_execution_workers(2, 1, Duration::from_millis(100));
    server.set_test_slow_inject_ms(500);

    let active = spawn_slow_request(addr.clone());
    for _ in 0..100 {
        if server.http_execution_active() == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.http_execution_active(), 1);

    let queued = send_request(&addr, "/__test/queued-timeout");
    assert!(
        queued.starts_with("HTTP/1.1 503"),
        "queued request should exhaust its handler deadline: {queued:?}"
    );
    assert!(queued.contains("handler deadline exceeded"));
    assert_eq!(
        server.http_execution_queued(),
        0,
        "timed-out waiter must release queue accounting"
    );

    let active_response = active.join().expect("active request thread");
    assert!(active_response.starts_with("HTTP/1.1 503"));
    server.set_test_slow_inject_ms(0);
    for _ in 0..100 {
        if server.http_limiter().current() == 0 && server.http_execution_active() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.http_limiter().current(), 0);
    assert_eq!(server.http_execution_active(), 0);
}
