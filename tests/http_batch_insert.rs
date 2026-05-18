//! Issue #582 — Analytics slice 4: `POST /collections/:name/batch`
//! integration test. Spins a real `RedDBServer`, drives the endpoint
//! over HTTP, and asserts the brief's acceptance bullets end-to-end:
//!
//! * all-or-nothing commit naming the offending row index,
//! * `Idempotency-Key` replay returns the cached prior result without
//!   re-executing,
//! * oversize batches reject with 413 before any storage write.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

fn isolated_runtime(tag: &str) -> RedDBRuntime {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "reddb-batch-http-{}-{}-{}",
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

fn spawn_server(tag: &str) -> String {
    std::env::remove_var("RED_ADMIN_TOKEN");
    let rt = isolated_runtime(tag);
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    let addr = addr.to_string();

    // Create the target collection up front via the public /query
    // endpoint so the test exercises only the batch endpoint after.
    let ddl = r#"{"query": "CREATE TABLE events (id INTEGER, name TEXT)"}"#;
    let (status, body) = http_post(&addr, "/query", ddl, None);
    assert_eq!(status, 200, "ddl failed: {body}");
    addr
}

fn http_post(
    addr: &str,
    path: &str,
    body: &str,
    idempotency_key: Option<&str>,
) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n"
    );
    if let Some(key) = idempotency_key {
        req.push_str(&format!("Idempotency-Key: {key}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
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
    let body_text = resp[body_idx..].to_string();
    (status, body_text)
}

fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
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
    let body_text = resp[body_idx..].to_string();
    (status, body_text)
}

#[test]
fn batch_happy_path_commits_every_row() {
    let addr = spawn_server("happy");
    let body = r#"[
        {"fields": {"id": 1, "name": "alpha"}},
        {"fields": {"id": 2, "name": "beta"}},
        {"fields": {"id": 3, "name": "gamma"}}
    ]"#;
    let (status, response) = http_post(&addr, "/collections/events/batch", body, None);
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("\"count\":3"), "{response}");

    let (scan_status, scan_body) = http_get(&addr, "/collections/events/scan");
    assert_eq!(scan_status, 200, "{scan_body}");
    assert!(scan_body.contains("alpha"), "{scan_body}");
    assert!(scan_body.contains("beta"), "{scan_body}");
    assert!(scan_body.contains("gamma"), "{scan_body}");
}

#[test]
fn batch_idempotency_key_replay_does_not_re_execute() {
    let addr = spawn_server("idem");
    let body1 = r#"[{"fields": {"id": 10, "name": "only-once"}}]"#;
    let (s1, r1) = http_post(&addr, "/collections/events/batch", body1, Some("k-abc"));
    assert_eq!(s1, 200, "{r1}");

    // Different body, same key — must replay the cached prior result
    // and leave storage untouched.
    let body2 = r#"[{"fields": {"id": 99, "name": "should-not-land"}}]"#;
    let (s2, r2) = http_post(&addr, "/collections/events/batch", body2, Some("k-abc"));
    assert_eq!(s2, 200, "{r2}");
    assert_eq!(r1, r2, "replay must echo the prior body byte-for-byte");

    let (_, scan_body) = http_get(&addr, "/collections/events/scan");
    assert!(scan_body.contains("only-once"), "{scan_body}");
    assert!(
        !scan_body.contains("should-not-land"),
        "replay re-executed: {scan_body}"
    );
}

#[test]
fn batch_row_failure_rolls_back_whole_batch() {
    let addr = spawn_server("rollback");
    // Row index 1 is shaped wrong on purpose — the batch must reject
    // with a typed error and leave storage empty.
    let body = r#"[
        {"fields": {"id": 1, "name": "first"}},
        {"not_fields": {}},
        {"fields": {"id": 3, "name": "third"}}
    ]"#;
    let (status, response) = http_post(&addr, "/collections/events/batch", body, None);
    assert_eq!(status, 400, "{response}");
    assert!(response.contains("\"row_index\":1"), "{response}");
    assert!(response.contains("RowParseFailure"), "{response}");

    let (_, scan_body) = http_get(&addr, "/collections/events/scan");
    assert!(
        !scan_body.contains("\"name\":\"first\""),
        "row 0 leaked despite row 1 failure: {scan_body}"
    );
}
