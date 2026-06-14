//! Runs `docs/examples/snowplow-adapter.mjs` against a real
//! RedDB HTTP server and asserts the resulting `events`
//! collection contains the expected rows. Keeps the example
//! from going stale — referenced by `docs/migrating-from-snowplow.md`.

#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

use reddb::server::RedDBServer;
use serde_json::{json, Value};

fn spawn_http_server() -> (support::TempDbFile, String) {
    let (db, rt) = support::persistent_runtime("snowplow-http");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    (db, addr.to_string())
}

fn post_query(addr: &str, query: &str) -> Value {
    let body = json!({ "query": query }).to_string();
    let request = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 for query {query:?}, got:\n{response}"
    );
    let body = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap();
    serde_json::from_str(body).expect("json body")
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn snowplow_adapter_example_ingests_events_end_to_end() {
    if !node_available() {
        eprintln!("skipping: `node` not available on PATH");
        return;
    }

    let (_db, addr) = spawn_http_server();
    let create = post_query(
        &addr,
        "CREATE TABLE events (event_id TEXT, collector_tstamp INTEGER, event_name TEXT, payload TEXT)",
    );
    assert_eq!(create["ok"].as_bool(), Some(true));

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = format!("{}/docs/examples/snowplow-adapter.mjs", manifest_dir);

    let output = Command::new("node")
        .arg(&example_path)
        .env("REDDB_URL", format!("http://{}", addr))
        .env("FLUSH_EVERY", "1")
        .output()
        .expect("spawn node");
    assert!(
        output.status.success(),
        "node example failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = post_query(
        &addr,
        "SELECT event_id, event_name FROM events ORDER BY event_id",
    );
    let records = rows["result"]["records"].as_array().expect("records");
    assert_eq!(records.len(), 2, "got rows: {rows}");
    assert_eq!(
        records[0]["values"]["event_id"].as_str(),
        Some("11111111-1111-1111-1111-111111111111")
    );
    assert_eq!(
        records[0]["values"]["event_name"].as_str(),
        Some("page_view")
    );
    assert_eq!(
        records[1]["values"]["event_id"].as_str(),
        Some("22222222-2222-2222-2222-222222222222")
    );
    assert_eq!(
        records[1]["values"]["event_name"].as_str(),
        Some("link_click")
    );
}
