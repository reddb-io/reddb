//! Integration tests for issue #934 (PRD #930, ADR 0035): per-principal
//! in-flight-request caps on the async HTTP edge.
//!
//! The async edge (#931) bounds *total* in-flight work through the global
//! limiter (async backpressure, no OS-thread cap). These tests cover the
//! *fairness* half added by #934: a single principal that exceeds its
//! per-principal concurrent-request cap is shed with a **structured 429
//! refusal** so clients can back off, while other principals — and health
//! probes — are unaffected.
//!
//! Determinism: the first request from a principal is held inside the
//! handler via the doc-hidden slow-downstream hook
//! (`set_test_slow_inject_ms`), which sleeps while the admission permits
//! are still held. A second request from the same principal therefore
//! arrives while the first occupies the only slot.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

/// Boot an async-edge HTTP server with the given per-principal cap. The
/// global limiter keeps its generous default so it never interferes with
/// these small-concurrency tests.
fn boot(max_conns_per_principal: usize) -> (String, RedDBServer) {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let server = RedDBServer::new(runtime).with_max_conns_per_principal(max_conns_per_principal);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    // Let the accept loop park on the listener.
    thread::sleep(Duration::from_millis(120));
    (addr, server)
}

/// Issue one request and return the full raw response text. `bearer`
/// selects the principal: distinct tokens hash to distinct principal
/// labels; `None` is the shared `anon` principal.
fn send_request(addr: &str, method: &str, path: &str, bearer: Option<&str>) -> String {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let auth = match bearer {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n"
    );
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Spawn a request on a background thread (used to hold a slot open while
/// the foreground thread probes the cap). Returns the join handle.
fn send_in_background(
    addr: String,
    method: &'static str,
    path: &'static str,
    bearer: Option<String>,
) -> thread::JoinHandle<String> {
    thread::spawn(move || send_request(&addr, method, path, bearer.as_deref()))
}

/// Block until exactly `n` principals hold a slot, or panic after a
/// generous timeout. Avoids racing the background holder's admission.
fn wait_tracked(server: &RedDBServer, n: usize) {
    for _ in 0..200 {
        if server.principal_conns().tracked_principals() == n {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for {n} tracked principals, saw {}",
        server.principal_conns().tracked_principals()
    );
}

#[test]
fn second_concurrent_request_from_same_principal_gets_structured_refusal() {
    let (addr, server) = boot(1);
    // Hold the first request inside the handler for ~1.2s, occupying the
    // single per-principal slot.
    server.set_test_slow_inject_ms(1_200);

    let holder = send_in_background(
        addr.clone(),
        "GET",
        "/health",
        Some("alice-secret-token".to_string()),
    );
    wait_tracked(&server, 1);

    // Second request from the *same* principal must be refused — it never
    // reaches the slow handler; it is shed at admission.
    let resp = send_request(&addr, "GET", "/health", Some("alice-secret-token"));

    assert!(
        resp.starts_with("HTTP/1.1 429"),
        "expected 429 status line, got: {resp:?}"
    );
    assert!(
        resp.to_ascii_lowercase().contains("retry-after:"),
        "expected Retry-After header, got: {resp:?}"
    );
    // Structured body lets the client branch on a stable code and read the
    // cap / live count for precise backoff.
    assert!(
        resp.contains("principal_connection_quota_exhausted"),
        "expected refusal code, got: {resp:?}"
    );
    assert!(resp.contains("\"ok\":false"), "expected ok:false, got: {resp:?}");
    assert!(resp.contains("\"limit\":1"), "expected limit, got: {resp:?}");
    assert!(
        resp.contains("\"principal\":"),
        "expected principal field, got: {resp:?}"
    );
    assert!(
        resp.contains("\"retry_after_secs\":"),
        "expected retry_after_secs field, got: {resp:?}"
    );

    // The refusal counter ticked exactly once.
    assert_eq!(server.principal_conns().rejected_total(), 1);

    // Release the holder and confirm the slot drains.
    let _ = holder.join();
    for _ in 0..200 {
        if server.principal_conns().tracked_principals() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.principal_conns().tracked_principals(), 0);
}

#[test]
fn distinct_principals_have_independent_budgets() {
    let (addr, server) = boot(1);
    server.set_test_slow_inject_ms(1_200);

    // Alice fills her single slot and keeps holding it (slow hook stays
    // armed) for the whole test.
    let alice = send_in_background(addr.clone(), "GET", "/health", Some("alice-token".to_string()));
    wait_tracked(&server, 1);

    // Bob has his own budget — admitted, not refused, even while Alice is
    // still at her cap. Bob's request runs through the slow handler too
    // (so Alice provably still holds her slot), then returns normally.
    let resp = send_request(&addr, "GET", "/health", Some("bob-token"));
    assert!(
        !resp.starts_with("HTTP/1.1 429"),
        "bob must not be refused by alice's cap, got: {resp:?}"
    );
    assert!(
        resp.starts_with("HTTP/1.1 2"),
        "bob's request should succeed, got: {resp:?}"
    );
    assert_eq!(
        server.principal_conns().rejected_total(),
        0,
        "no refusal expected across distinct principals"
    );

    let _ = alice.join();
}

#[test]
fn health_probes_are_exempt_from_per_principal_cap() {
    let (addr, server) = boot(1);
    server.set_test_slow_inject_ms(1_200);

    // An anonymous non-health request fills the single anon slot.
    let holder = send_in_background(addr.clone(), "GET", "/health", None);
    wait_tracked(&server, 1);

    // A second anon non-health request is refused (sanity: the cap is live
    // for the anon principal).
    let refused = send_request(&addr, "GET", "/health", None);
    assert!(
        refused.starts_with("HTTP/1.1 429"),
        "anon over-cap request should be refused, got: {refused:?}"
    );

    // But a health probe — same anon principal — is exempt and succeeds.
    server.set_test_slow_inject_ms(0);
    let health = send_request(&addr, "GET", "/health/live", None);
    assert!(
        !health.starts_with("HTTP/1.1 429"),
        "health probe must never be refused by the per-principal cap, got: {health:?}"
    );
    assert!(
        health.starts_with("HTTP/1.1 2"),
        "health probe should succeed, got: {health:?}"
    );

    let _ = holder.join();
}

#[test]
fn disabled_cap_admits_unlimited_concurrency_from_one_principal() {
    // Cap 0 (the default) disables enforcement: many concurrent requests
    // from one principal all proceed, none refused.
    let (addr, server) = boot(0);
    server.set_test_slow_inject_ms(400);

    let mut handles = Vec::new();
    for _ in 0..4 {
        handles.push(send_in_background(
            addr.clone(),
            "GET",
            "/health",
            Some("same-token".to_string()),
        ));
    }
    for h in handles {
        let resp = h.join().expect("join");
        assert!(
            !resp.starts_with("HTTP/1.1 429"),
            "disabled cap must not refuse, got: {resp:?}"
        );
    }
    assert_eq!(server.principal_conns().rejected_total(), 0);
    assert_eq!(server.principal_conns().tracked_principals(), 0);
}
