//! Integration test for issue #934 (PRD #930): the async HTTP edge must
//! bound any *single* principal's concurrent in-flight requests and refuse
//! over-cap requests with a structured `429` that a client can back off on
//! — while leaving the global cap (total backpressure) and other principals
//! untouched.
//!
//! Determinism: a per-principal cap of 1 plus a slow-inject hook (the same
//! test hook the deadline test uses) holds one request in flight long enough
//! to fire follow-up requests against a parked permit, so no real load race
//! is needed.

#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;

/// Send one buffered `POST /query` carrying `Authorization: Bearer <token>`
/// and return the full raw response text. The body is a trivial `SELECT 1`
/// so the engine call is cheap; the slow-inject hook (not the query) is what
/// keeps the request in flight.
fn post_query(addr: &str, token: &str) -> String {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = br#"{"query":"SELECT 1"}"#;
    let request = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    tcp.write_all(body).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

fn boot(cap: usize, slow_ms: u64) -> (support::TempDbFile, String, RedDBServer) {
    let (db, runtime) = support::persistent_runtime("principal-inflight-http");
    let server = RedDBServer::new(runtime).with_principal_inflight_cap(cap);
    server.set_test_slow_inject_ms(slow_ms);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let bg = server.clone();
    bg.serve_in_background_on(listener);
    thread::sleep(Duration::from_millis(80));
    (db, addr, server)
}

#[test]
fn over_cap_principal_gets_structured_429_others_unaffected_then_recovers() {
    // cap=1 per principal; each request parks ~700ms in flight.
    let (_db, addr, server) = boot(1, 700);

    // Fire request A for principal `alice` in the background; it acquires
    // the single per-principal slot and holds it while the slow-inject
    // sleeps inside the engine call.
    let addr_a = addr.clone();
    let a = thread::spawn(move || post_query(&addr_a, "alice-token"));

    // Wait until A is actually in flight (its permit is held). One
    // principal tracked == A occupies its slot.
    let mut in_flight = false;
    for _ in 0..200 {
        if server.principal_limiter().tracked_principals() >= 1 {
            in_flight = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(in_flight, "request A should be in flight holding its slot");

    // Request B for the SAME principal must be refused: alice is at her
    // cap of 1. Structured 429 with Retry-After and the backoff fields.
    let b = post_query(&addr, "alice-token");
    assert!(
        b.starts_with("HTTP/1.1 429"),
        "over-cap same-principal request must be 429, got: {b:?}"
    );
    assert!(
        b.to_ascii_lowercase().contains("retry-after:"),
        "refusal must carry Retry-After, got: {b:?}"
    );
    assert!(
        b.contains("principal_inflight_exhausted"),
        "refusal must carry the structured code, got: {b:?}"
    );
    assert!(
        b.contains("\"limit\":1"),
        "refusal must report the principal's limit, got: {b:?}"
    );

    // Request C for a DIFFERENT principal shares no budget with alice and
    // must be admitted (it routes normally — any non-429 from the limiter).
    let c = post_query(&addr, "bob-token");
    assert!(
        !c.starts_with("HTTP/1.1 429"),
        "a different principal must not be throttled by alice's cap, got: {c:?}"
    );

    // A completes; alice's slot frees. The metric counter saw exactly the
    // one refusal (B) — C and D were admitted, not refused.
    let _ = a.join().unwrap();
    assert_eq!(
        server.principal_limiter().rejected_total(),
        1,
        "exactly one over-cap refusal expected"
    );

    // Recovery: with alice's slot drained, a fresh alice request is
    // admitted again (not a limiter 429).
    let d = post_query(&addr, "alice-token");
    assert!(
        !d.starts_with("HTTP/1.1 429"),
        "alice should be admitted again after her slot drained, got: {d:?}"
    );
}

#[test]
fn disabled_cap_admits_unlimited_concurrency_per_principal() {
    // cap=0 disables the per-principal gate entirely; many concurrent
    // requests for one principal all get through (bounded only by the
    // global cap, which is the default and far above this handful).
    let (_db, addr, _server) = boot(0, 300);

    let mut handles = Vec::new();
    for _ in 0..6 {
        let addr = addr.clone();
        handles.push(thread::spawn(move || post_query(&addr, "same-token")));
    }
    for h in handles {
        let resp = h.join().unwrap();
        assert!(
            !resp.starts_with("HTTP/1.1 429"),
            "disabled per-principal cap must not throttle, got: {resp:?}"
        );
    }
}
