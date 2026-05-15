use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn only_record(result: &reddb::runtime::RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(result.result.records.len(), 1, "expected one row");
    &result.result.records[0]
}

fn text_field<'a>(record: &'a UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn uint_field(record: &UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected {field} unsigned integer field, got {other:?} in {record:?}"),
    }
}

fn assert_row_envelope(record: &UnifiedRecord, collection: &str) -> u64 {
    let rid = uint_field(record, "rid");
    assert_eq!(text_field(record, "collection"), collection);
    assert_eq!(text_field(record, "kind"), "row");
    assert_eq!(record.get("tenant"), Some(&Value::Null));
    assert!(record.get("created_at").is_some(), "missing created_at");
    assert!(record.get("updated_at").is_some(), "missing updated_at");
    assert!(
        record.get("red_entity_id").is_none(),
        "public row envelope should not expose red_entity_id: {record:?}"
    );
    rid
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

fn http_post_query(addr: &str, query: &str) -> (u16, serde_json::Value) {
    let body = format!(r#"{{"query":{}}}"#, serde_json::to_string(query).unwrap());
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    let req = format!(
        "POST /query HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
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
fn row_select_and_returning_expose_public_rid_envelope() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE row_items (id INT, name TEXT)");

    let inserted = exec(
        &rt,
        "INSERT INTO row_items (id, name) VALUES (1, 'alpha') RETURNING *",
    );
    let inserted_rid = assert_row_envelope(only_record(&inserted), "row_items");

    let selected = exec(&rt, "SELECT * FROM row_items WHERE id = 1");
    let selected_rid = assert_row_envelope(only_record(&selected), "row_items");
    assert_eq!(selected_rid, inserted_rid);

    let updated = exec(
        &rt,
        &format!("UPDATE row_items SET name = 'beta' WHERE rid = {inserted_rid} RETURNING *"),
    );
    let updated_record = only_record(&updated);
    assert_eq!(
        assert_row_envelope(updated_record, "row_items"),
        inserted_rid
    );
    assert_eq!(text_field(updated_record, "name"), "beta");
}

#[test]
fn http_query_json_uses_rid_for_row_identity() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE http_rows (id INT, name TEXT)");
    exec(&rt, "INSERT INTO http_rows (id, name) VALUES (1, 'alpha')");
    let addr = spawn_http(rt);

    let (status, body) = http_post_query(&addr, "SELECT * FROM http_rows WHERE id = 1");

    assert_eq!(status, 200, "body = {body}");
    assert_eq!(body["ok"], true);
    assert!(body["result"]["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .any(|column| column.as_str() == Some("rid")));
    let values = &body["result"]["records"][0]["values"];
    assert!(values["rid"].as_u64().is_some(), "body = {body}");
    assert_eq!(values["collection"], "http_rows");
    assert_eq!(values["kind"], "row");
    assert_eq!(values["tenant"], serde_json::Value::Null);
    assert!(values.get("red_entity_id").is_none(), "body = {body}");
}
