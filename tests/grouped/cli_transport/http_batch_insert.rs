//! Issue #582 — Analytics slice 4: `POST /collections/:name/batch`
//! integration test. Spins a real `RedDBServer`, drives the endpoint
//! over HTTP, and asserts the brief's acceptance bullets end-to-end:
//!
//! * all-or-nothing commit naming the offending row index,
//! * `Idempotency-Key` replay returns the committed prior result without
//!   re-executing,
//! * oversize batches reject with 413 before any storage write.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use reddb::server::RedDBServer;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::{RedDBOptions, RedDBRuntime};

fn isolated_runtime(tag: &str) -> (support::TempDataDir, RedDBRuntime) {
    let dir = support::temp_data_dir(&format!("batch-http-{tag}"));
    let opts = RedDBOptions::in_memory().with_data_path(dir.join("data.rdb"));
    let rt = RedDBRuntime::with_options(opts).expect("runtime");
    (dir, rt)
}

fn spawn_server(tag: &str) -> (support::TempDataDir, String) {
    std::env::remove_var("RED_ADMIN_TOKEN");
    let (dir, rt) = isolated_runtime(tag);
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
    (dir, addr)
}

struct ServerProcess(Child);

impl ServerProcess {
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_server_process(path: &Path, port: u16) -> ServerProcess {
    let child = Command::new(env!("CARGO_BIN_EXE_red"))
        .args([
            "server",
            "--path",
            path.to_str().expect("utf-8 database path"),
            "--http",
            "--http-bind",
            &format!("127.0.0.1:{port}"),
            "--storage-preset",
            "primary-replica-dev",
            "--no-auth",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn red server");
    ServerProcess(child)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral address")
        .port()
}

fn wait_until_listening(process: &mut ServerProcess, addr: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            process.0.try_wait().expect("poll red server").is_none(),
            "red server exited before binding {addr}"
        );
        if TcpStream::connect(addr).is_ok() {
            let (status, _) = http_get(addr, "/health/ready");
            if status == 200 {
                return;
            }
        }
        assert!(Instant::now() < deadline, "red server did not bind {addr}");
        thread::sleep(Duration::from_millis(25));
    }
}

fn http_post(addr: &str, path: &str, body: &str, idempotency_key: Option<&str>) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
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
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
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

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn batch_happy_path_commits_every_row() {
    let (_dir, addr) = spawn_server("happy");
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
    let (_dir, addr) = spawn_server("idem");
    let body1 = r#"[{"fields": {"id": 10, "name": "only-once"}}]"#;
    let (s1, r1) = http_post(&addr, "/collections/events/batch", body1, Some("k-abc"));
    assert_eq!(s1, 200, "{r1}");

    // Different body, same key — must replay the committed prior result
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
fn concurrent_retries_with_one_key_commit_once() {
    let (_dir, addr) = spawn_server("concurrent-idem");
    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));
    let mut handles = Vec::new();
    for id in 0..workers {
        let addr = addr.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let body = format!(r#"[{{"fields": {{"id": {id}, "name": "writer-{id}"}}}}]"#);
            barrier.wait();
            http_post(
                &addr,
                "/collections/events/batch",
                &body,
                Some("concurrent-retry-token"),
            )
        }));
    }

    let responses: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("batch request thread"))
        .collect();
    assert!(
        responses.iter().all(|(status, _)| *status == 200),
        "all retries must observe success: {responses:?}"
    );
    assert!(
        responses.iter().all(|(_, body)| body == &responses[0].1),
        "all retries must replay one response: {responses:?}"
    );

    let (scan_status, scan_body) = http_get(&addr, "/collections/events/scan");
    assert_eq!(scan_status, 200, "{scan_body}");
    assert!(
        scan_body.contains("\"total\":1"),
        "concurrent retries committed more than one row: {scan_body}"
    );
}

#[test]
fn same_key_in_two_databases_does_not_cross_contaminate() {
    let (_first_dir, first_addr) = spawn_server("idem-db-one");
    let (_second_dir, second_addr) = spawn_server("idem-db-two");
    let key = Some("same-token-different-databases");

    let (first_status, _) = http_post(
        &first_addr,
        "/collections/events/batch",
        r#"[{"fields": {"id": 1, "name": "database-one"}}]"#,
        key,
    );
    let (second_status, _) = http_post(
        &second_addr,
        "/collections/events/batch",
        r#"[{"fields": {"id": 2, "name": "database-two"}}]"#,
        key,
    );
    assert_eq!(first_status, 200);
    assert_eq!(second_status, 200);

    let (_, first_scan) = http_get(&first_addr, "/collections/events/scan");
    let (_, second_scan) = http_get(&second_addr, "/collections/events/scan");
    assert!(first_scan.contains("database-one"), "{first_scan}");
    assert!(!first_scan.contains("database-two"), "{first_scan}");
    assert!(second_scan.contains("database-two"), "{second_scan}");
    assert!(!second_scan.contains("database-one"), "{second_scan}");
}

#[test]
fn committed_batch_retry_on_restarted_process_replays_without_duplicate() {
    let dir = support::temp_data_dir("batch-http-durable-idempotency");
    let path = dir.join("data.rdb");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let mut first = spawn_server_process(&path, port);
    wait_until_listening(&mut first, &addr);
    let (ddl_status, ddl_body) = http_post(
        &addr,
        "/query",
        r#"{"query": "CREATE TABLE events (id INTEGER, name TEXT)"}"#,
        None,
    );
    assert_eq!(ddl_status, 200, "{ddl_body}");

    let first_body = r#"[{"fields": {"id": 10, "name": "committed-before-response-loss"}}]"#;
    let (first_status, first_response) = http_post(
        &addr,
        "/collections/events/batch",
        first_body,
        Some("lost-response-token"),
    );
    assert_eq!(first_status, 200, "{first_response}");

    let wal_path = reddb_file::unified_wal_path(&path);
    let atomic_batch = WalReader::open(&wal_path)
        .expect("open database WAL")
        .iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("decode database WAL")
        .into_iter()
        .find_map(|(_, record)| match record {
            WalRecord::TxCommitBatch { actions, .. }
                if actions
                    .iter()
                    .any(|action| contains_bytes(action, b"committed-before-response-loss")) =>
            {
                Some(actions)
            }
            _ => None,
        })
        .expect("batch insert must be represented by one transaction commit record");
    assert!(
        atomic_batch
            .iter()
            .any(|action| contains_bytes(action, b"red.batch.idempotency")),
        "the committed rows and durable idempotency receipt must share one TxCommitBatch"
    );

    // The client loses the successful response and retries through another
    // process after the first writer disappears.
    first.stop();

    let mut second = spawn_server_process(&path, port);
    wait_until_listening(&mut second, &addr);
    let retry_body = r#"[{"fields": {"id": 99, "name": "must-not-be-inserted"}}]"#;
    let (retry_status, retry_response) = http_post(
        &addr,
        "/collections/events/batch",
        retry_body,
        Some("lost-response-token"),
    );
    assert_eq!(retry_status, 200, "{retry_response}");
    assert_eq!(
        retry_response, first_response,
        "a retry on another process must replay the committed response"
    );

    let (scan_status, scan_body) = http_get(&addr, "/collections/events/scan");
    assert_eq!(scan_status, 200, "{scan_body}");
    assert!(
        scan_body.contains("committed-before-response-loss"),
        "{scan_body}"
    );
    assert!(
        !scan_body.contains("must-not-be-inserted"),
        "retry duplicated the committed operation after restart: {scan_body}"
    );
}

#[test]
fn batch_row_failure_rolls_back_whole_batch() {
    let (_dir, addr) = spawn_server("rollback");
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
