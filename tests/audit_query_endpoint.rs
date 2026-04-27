//! End-to-end smoke for `GET /admin/audit`.
//!
//! Spins a `RedDBServer`, emits a handful of audit events through the
//! runtime's `AuditLogger`, then issues filtered HTTP requests and
//! confirms the response matches the in-memory expectation.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::runtime::audit_log::{AuditAuthSource, AuditEvent, Outcome};
use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

/// Unique per-test data path so the audit logger doesn't share
/// `<tmp>/.audit.log` across parallel tests.
fn isolated_runtime(tag: &str) -> RedDBRuntime {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "reddb-audit-endpoint-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let opts = RedDBOptions::in_memory().with_data_path(dir.join("data.rdb"));
    RedDBRuntime::with_options(opts).expect("runtime")
}

fn spawn_http(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    addr.to_string()
}

fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let resp = String::from_utf8_lossy(&buf).to_string();
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_idx = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    let body = resp[body_idx..].to_string();
    (status, body)
}

fn seed_events(rt: &RedDBRuntime) {
    // Make sure RED_ADMIN_TOKEN isn't set across the test surface
    // (other tests may have set it).
    std::env::remove_var("RED_ADMIN_TOKEN");

    let logger = rt.audit_log();
    logger.record_event(
        AuditEvent::builder("auth/login.ok")
            .principal("alice")
            .source(AuditAuthSource::Password)
            .tenant("acme")
            .outcome(Outcome::Success)
            .build(),
    );
    logger.record_event(
        AuditEvent::builder("auth/login.ok")
            .principal("alice")
            .source(AuditAuthSource::Password)
            .tenant("acme")
            .outcome(Outcome::Success)
            .build(),
    );
    logger.record_event(
        AuditEvent::builder("auth/login.deny")
            .principal("eve")
            .source(AuditAuthSource::Password)
            .tenant("acme")
            .outcome(Outcome::Denied)
            .build(),
    );
    logger.record_event(
        AuditEvent::builder("admin/shutdown")
            .principal("alice")
            .source(AuditAuthSource::Session)
            .tenant("acme")
            .outcome(Outcome::Success)
            .build(),
    );
    assert!(logger.wait_idle(Duration::from_secs(3)));
}

#[test]
fn query_by_principal_returns_only_alice_events() {
    let rt = isolated_runtime("seed");
    seed_events(&rt);
    let addr = spawn_http(rt);

    let (status, body) = http_get(&addr, "/admin/audit?principal=alice");
    assert_eq!(status, 200, "body = {body}");
    assert!(body.contains("\"count\":3"), "body = {body}");
    assert!(body.contains("\"principal\":\"alice\""));
    assert!(!body.contains("\"principal\":\"eve\""));
}

#[test]
fn query_by_action_prefix_filters_correctly() {
    let rt = isolated_runtime("seed");
    seed_events(&rt);
    let addr = spawn_http(rt);

    let (status, body) = http_get(&addr, "/admin/audit?action=auth/");
    assert_eq!(status, 200, "body = {body}");
    assert!(body.contains("\"count\":3"), "body = {body}");
    assert!(body.contains("auth/login.ok"));
    assert!(body.contains("auth/login.deny"));
    assert!(!body.contains("admin/shutdown"));
}

#[test]
fn query_by_outcome_denied_returns_eve_only() {
    let rt = isolated_runtime("seed");
    seed_events(&rt);
    let addr = spawn_http(rt);

    let (status, body) = http_get(&addr, "/admin/audit?outcome=denied");
    assert_eq!(status, 200, "body = {body}");
    assert!(body.contains("\"count\":1"), "body = {body}");
    assert!(body.contains("\"principal\":\"eve\""));
}

#[test]
fn query_jsonl_format_returns_ndjson() {
    let rt = isolated_runtime("seed");
    seed_events(&rt);
    let addr = spawn_http(rt);

    let (status, body) = http_get(&addr, "/admin/audit?action=auth/&format=jsonl");
    assert_eq!(status, 200);
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "ndjson body = {body}");
    for line in &lines {
        // Each line is parseable JSON.
        let _: reddb::json::Value =
            reddb::json::from_str(line).unwrap_or_else(|e| panic!("bad jsonl: {e}: {line}"));
    }
}

#[test]
fn invalid_outcome_returns_400() {
    let rt = isolated_runtime("seed");
    seed_events(&rt);
    let addr = spawn_http(rt);

    let (status, _body) = http_get(&addr, "/admin/audit?outcome=banana");
    assert_eq!(status, 400);
}
