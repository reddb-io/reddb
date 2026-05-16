use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn read_event_payload(rt: &RedDBRuntime, queue: &str) -> serde_json::Value {
    let result = exec(
        rt,
        &format!("QUEUE READ {queue} GROUP evt_readers CONSUMER c1 COUNT 1"),
    );
    let record = result
        .result
        .records
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no event in queue {queue}"));
    match record.get("payload") {
        Some(Value::Json(bytes)) => {
            serde_json::from_slice(bytes).expect("event payload should be valid JSON")
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}

fn spawn_http(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    thread::spawn(move || {
        let _ = server.serve_one_on(listener);
    });
    addr
}

fn http_get(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    let req = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_idx = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    let json = serde_json::from_str(&resp[body_idx..])
        .unwrap_or_else(|err| panic!("HTTP body should be JSON: {err}: {resp}"));
    (status, json)
}

#[test]
fn row_event_payload_uses_public_item_identity() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT) WITH EVENTS");
    exec(&rt, "QUEUE GROUP CREATE users_events evt_readers");

    let inserted = exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (42, 'ada@example.com') RETURNING rid",
    );
    let rid = match inserted.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected rid field, got {other:?}"),
    };

    let payload = read_event_payload(&rt, "users_events");

    assert_eq!(payload["collection"], "users");
    assert_eq!(payload["kind"], "row");
    assert_eq!(payload["rid"].as_u64(), Some(rid));
    assert!(payload.get("entity_id").is_none(), "payload = {payload}");
    assert!(payload.get("entity_kind").is_none(), "payload = {payload}");
}

#[test]
fn row_delete_event_payload_uses_public_item_identity() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT) WITH EVENTS");
    exec(&rt, "QUEUE GROUP CREATE users_events evt_readers");

    let inserted = exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (7, 'del@example.com') RETURNING rid",
    );
    let rid = match inserted.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected rid field, got {other:?}"),
    };
    // drain insert event
    let _ = read_event_payload(&rt, "users_events");

    exec(&rt, "DELETE FROM users WHERE id = 7");
    let payload = read_event_payload(&rt, "users_events");

    assert_eq!(payload["op"], "delete");
    assert_eq!(payload["collection"], "users");
    assert_eq!(payload["kind"], "row");
    assert_eq!(payload["rid"].as_u64(), Some(rid));
    assert!(payload.get("entity_id").is_none(), "payload = {payload}");
    assert!(payload.get("entity_kind").is_none(), "payload = {payload}");
}

#[test]
fn cdc_changes_payload_uses_public_item_identity_for_kv() {
    let rt = runtime();
    let inserted = exec(
        &rt,
        "INSERT INTO config KV (key, value) VALUES ('feature', 'enabled') RETURNING rid",
    );
    let rid = match inserted.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected rid field, got {other:?}"),
    };
    let addr = spawn_http(rt);

    let (status, body) = http_get(&addr, "/changes?since_lsn=0&limit=10");

    assert_eq!(status, 200, "body = {body}");
    assert_eq!(body["ok"], true);
    let event = body["events"]
        .as_array()
        .and_then(|events| events.last())
        .expect("cdc event");
    assert_eq!(event["operation"], "insert");
    assert_eq!(event["collection"], "config");
    assert_eq!(event["kind"], "kv");
    assert_eq!(event["rid"].as_u64(), Some(rid));
    assert!(event.get("entity_id").is_none(), "body = {body}");
    assert!(event.get("entity_kind").is_none(), "body = {body}");
}
