//! Issue #1243 (PRD #1237 Phase B) — primary↔replica reconnect telemetry.
//!
//! A replica's pull loop persists `connecting` when the link to its primary
//! drops and `healthy` when a pull succeeds again. Those transitions flow
//! through the runtime's reconnect tracker, which the `/metrics` endpoint
//! exports as `reddb_replication_reconnects_total` and the status read
//! model surfaces for red-ui.
//!
//! Standing up two real gRPC nodes and tearing down a live TCP link mid-pull
//! would be slow and flaky on the memory-constrained drain host, so this
//! test drives the *exact* production signal the loop emits — the persisted
//! link-state transitions — through the runtime's public observe seam, then
//! asserts the counter end-to-end over the real `/metrics` HTTP surface.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer};

struct HttpReply {
    status: u16,
    body: String,
}

fn http_get(addr: SocketAddr, path: &str) -> HttpReply {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).expect("write request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http framing");
    let head = String::from_utf8_lossy(&raw[..split]);
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status");
    let body = String::from_utf8_lossy(&raw[split + 4..]).to_string();
    HttpReply { status, body }
}

/// Pull the integer value of `reddb_replication_reconnects_total` out of an
/// OpenMetrics body, tolerating the optional `{replica_id="…"}` label.
fn reconnects_total(body: &str) -> u64 {
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("reddb_replication_reconnects_total") {
            // `rest` is either ` <n>` or `{labels} <n>`.
            let value = rest.rsplit(' ').next().expect("metric value");
            return value.trim().parse().expect("counter value parses");
        }
    }
    panic!("reddb_replication_reconnects_total not found in /metrics body:\n{body}");
}

/// Scrape `/metrics` over HTTP, assert a 200, and return the current value of
/// `reddb_replication_reconnects_total`.
fn scrape_reconnects(addr: SocketAddr) -> u64 {
    let reply = http_get(addr, "/metrics");
    assert_eq!(reply.status, 200, "/metrics must answer 200");
    reconnects_total(&reply.body)
}

#[test]
fn reconnect_counter_increments_once_per_link_drop_and_restore() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    // The runtime is `Clone` (Arc inner); keep a handle to drive link-state
    // transitions while the server clone serves `/metrics`.
    let handle = runtime.clone();
    let server = RedDBServer::new(runtime);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("server addr");
    let _join = server.serve_in_background_on(listener);

    // Give the background edge a moment to start accepting connections.
    let metrics = poll_metrics(addr);
    assert_eq!(
        reconnects_total(&metrics),
        0,
        "counter starts at zero before any link activity"
    );

    // Initial connect — `connecting` then `healthy`. This is the first time
    // the link comes up; it must NOT count as a reconnect.
    handle.observe_replica_link_state("connecting");
    handle.observe_replica_link_state("healthy");
    assert_eq!(
        scrape_reconnects(addr),
        0,
        "the initial connect is not a reconnect"
    );

    // Drop the link (a failed pull falls back to `connecting`) and restore
    // it (`healthy`). That is one reconnect.
    handle.observe_replica_link_state("connecting");
    handle.observe_replica_link_state("healthy");
    assert_eq!(
        scrape_reconnects(addr),
        1,
        "one drop+restore increments the counter exactly once"
    );

    // A multi-poll outage persists `connecting` several times but is still a
    // single reconnect when the link recovers.
    handle.observe_replica_link_state("connecting");
    handle.observe_replica_link_state("connecting");
    handle.observe_replica_link_state("healthy");
    assert_eq!(
        scrape_reconnects(addr),
        2,
        "a multi-poll outage counts once, not once per failed poll"
    );

    // The runtime accessor agrees with the exported series.
    assert_eq!(handle.replication_reconnects_count(), 2);
}

/// The edge thread builds its own tokio runtime before it can accept; retry
/// the first scrape briefly so the test is not racy on a loaded host.
fn poll_metrics(addr: SocketAddr) -> String {
    for _ in 0..50 {
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let request = "GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
            if stream.write_all(request.as_bytes()).is_ok() {
                let _ = stream.shutdown(Shutdown::Write);
                let mut raw = Vec::new();
                if stream.read_to_end(&mut raw).is_ok() && !raw.is_empty() {
                    let text = String::from_utf8_lossy(&raw);
                    if let Some(idx) = text.find("\r\n\r\n") {
                        return text[idx + 4..].to_string();
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not serve /metrics within the poll window");
}
