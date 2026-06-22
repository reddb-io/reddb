//! Regression coverage for issue #545.
//!
//! Pins the three contract bullets from #545 (parent #449):
//!
//! 1. An **explicit** listener bind failure aborts startup — the boot
//!    path receives an `Err` with the underlying reason. The failure
//!    is also recorded in the shared [`TransportReadiness`] so any
//!    operator-side log/diagnostic surface can see it.
//! 2. An **implicit / default** listener bind failure degrades — the
//!    boot path receives `Ok(None)` (no listener bound) and the
//!    failure is recorded in `readiness.failed`. The server keeps
//!    running on the listeners that *did* bind.
//! 3. The HTTP `/health` endpoint enumerates both `active` and
//!    `failed` listeners under `transport_listeners`, with the per-
//!    listener fields the brief calls out (`transport`, `bind_addr`,
//!    `explicit`, plus `reason` for failures).
//!
//! The behavior already shipped under #449 — this file pins the
//! contract down with a dedicated regression file traceable to #545,
//! mirroring the discipline used by the #540 / #541 / #542 / #543 /
//! #544 regression commits.

#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::{RedDBServer, ServerOptions};
use reddb::service_cli::{
    bind_listener_for_startup, TransportListenerFailure, TransportListenerState, TransportReadiness,
};
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

/// Bind a throwaway listener and return its `addr:port` so a second
/// bind attempt at the same address provokes the OS error
/// `bind_listener_for_startup` is supposed to classify.
fn occupy_random_port() -> (TcpListener, String) {
    let occupier = TcpListener::bind("127.0.0.1:0").expect("seed bind");
    let addr = occupier
        .local_addr()
        .expect("seed listener local_addr")
        .to_string();
    (occupier, addr)
}

fn spawn_http_server(readiness: TransportReadiness) -> (support::TempDbFile, String) {
    let mut options = ServerOptions::default();
    options.transport_readiness = readiness;
    let (db, rt) = support::persistent_runtime("transport-readiness-http");
    let server = RedDBServer::with_options(rt, options);
    let listener = TcpListener::bind("127.0.0.1:0").expect("http listener bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    (db, addr.to_string())
}

fn http_get(addr: &str, path: &str) -> (u16, JsonValue) {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let parsed = serde_json::from_str(body).unwrap_or_else(|_| json!({ "raw": body }));
    (status, parsed)
}

#[test]
fn explicit_bind_failure_is_fatal_and_recorded() {
    let (_occupier, busy_addr) = occupy_random_port();
    let mut readiness = TransportReadiness::default();

    let result = bind_listener_for_startup(&mut readiness, "http", &busy_addr, true);

    let err = result.expect_err("explicit bind failure must propagate as Err");
    assert!(
        err.contains("explicit"),
        "fatal error string should mark the bind as explicit: {err}"
    );
    assert!(
        err.contains(&busy_addr),
        "fatal error string should include the bind address: {err}"
    );
    assert!(
        readiness.active.is_empty(),
        "no listener should be recorded active on bind failure: {:?}",
        readiness.active
    );
    assert_eq!(
        readiness.failed.len(),
        1,
        "the failed bind should be recorded for operator diagnostics: {:?}",
        readiness.failed
    );
    let failure = &readiness.failed[0];
    assert_eq!(failure.transport, "http");
    assert_eq!(failure.bind_addr, busy_addr);
    assert!(
        failure.explicit,
        "failure must remember it came from an explicit operator request"
    );
    assert!(
        !failure.reason.is_empty(),
        "failure must carry an operator-readable reason"
    );
}

#[test]
fn implicit_bind_failure_degrades_and_does_not_block_other_listeners() {
    let (_occupier, busy_addr) = occupy_random_port();
    let mut readiness = TransportReadiness::default();

    // Implicit/default listener: the OS-busy address must not abort
    // startup. The function returns Ok(None) and records the failure.
    let degraded = bind_listener_for_startup(&mut readiness, "wire", &busy_addr, false)
        .expect("implicit bind failure must not surface as Err");
    assert!(
        degraded.is_none(),
        "no listener should be produced for a failed implicit bind"
    );
    assert_eq!(readiness.failed.len(), 1, "{:?}", readiness.failed);
    let failure = &readiness.failed[0];
    assert_eq!(failure.transport, "wire");
    assert_eq!(failure.bind_addr, busy_addr);
    assert!(
        !failure.explicit,
        "failure must remember it came from an implicit default"
    );
    assert!(
        !failure.reason.is_empty(),
        "implicit failure still needs an operator-readable reason"
    );

    // A subsequent listener at a fresh address still binds — the
    // earlier degrade must not poison the readiness state.
    let healthy = bind_listener_for_startup(&mut readiness, "grpc", "127.0.0.1:0", false)
        .expect("a free address must bind cleanly after a prior degrade")
        .expect("a successful bind must yield a TcpListener");
    let local = healthy.local_addr().expect("healthy local_addr");
    assert_eq!(readiness.active.len(), 1, "{:?}", readiness.active);
    let active = &readiness.active[0];
    assert_eq!(active.transport, "grpc");
    assert_eq!(active.bind_addr, "127.0.0.1:0");
    assert!(!active.explicit);
    // Sanity: the recorded readiness coexists with the actual bound
    // socket, so the server can still accept connections on it.
    assert!(local.port() > 0);
}

#[test]
fn health_endpoint_enumerates_active_and_failed_listeners() {
    let readiness = TransportReadiness {
        active: vec![
            TransportListenerState {
                transport: "http".to_string(),
                bind_addr: "127.0.0.1:55055".to_string(),
                explicit: true,
            },
            TransportListenerState {
                transport: "grpc".to_string(),
                bind_addr: "127.0.0.1:55055".to_string(),
                explicit: false,
            },
        ],
        failed: vec![TransportListenerFailure {
            transport: "wire".to_string(),
            bind_addr: "127.0.0.1:6378".to_string(),
            explicit: false,
            reason: "wire listener bind 127.0.0.1:6378: address in use".to_string(),
        }],
    };
    let (_db, addr) = spawn_http_server(readiness);

    let (status, body) = http_get(&addr, "/health");
    // `/health` may answer 200 (ready) or 503 (still warming up) at
    // the time the test connects — issue #545 only requires that the
    // body enumerates the transport state in either case.
    assert!(
        matches!(status, 200 | 503),
        "/health should answer 200 or 503, got {status} body={body}"
    );

    let listeners = body
        .get("transport_listeners")
        .expect("/health JSON must include transport_listeners");
    let active = listeners
        .get("active")
        .and_then(JsonValue::as_array)
        .expect("transport_listeners.active must be an array");
    let failed = listeners
        .get("failed")
        .and_then(JsonValue::as_array)
        .expect("transport_listeners.failed must be an array");

    assert_eq!(
        active.len(),
        2,
        "active listeners should enumerate verbatim"
    );
    assert_eq!(
        failed.len(),
        1,
        "failed listeners should enumerate verbatim"
    );

    let http_entry = active
        .iter()
        .find(|entry| entry.get("transport").and_then(JsonValue::as_str) == Some("http"))
        .expect("http listener should appear under active");
    assert_eq!(
        http_entry.get("bind_addr").and_then(JsonValue::as_str),
        Some("127.0.0.1:55055")
    );
    assert_eq!(
        http_entry.get("explicit").and_then(JsonValue::as_bool),
        Some(true),
        "explicit flag must survive serialization so operators can tell whether the listener was operator-requested"
    );

    let wire_entry = &failed[0];
    assert_eq!(
        wire_entry.get("transport").and_then(JsonValue::as_str),
        Some("wire")
    );
    assert_eq!(
        wire_entry.get("bind_addr").and_then(JsonValue::as_str),
        Some("127.0.0.1:6378")
    );
    assert_eq!(
        wire_entry.get("explicit").and_then(JsonValue::as_bool),
        Some(false)
    );
    let reason = wire_entry
        .get("reason")
        .and_then(JsonValue::as_str)
        .expect("failed listeners must surface a reason");
    assert!(
        reason.contains("127.0.0.1:6378"),
        "reason should echo the bind address so the operator can correlate logs: {reason}"
    );
}
