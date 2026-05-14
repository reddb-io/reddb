use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn int(row: &reddb::storage::query::UnifiedRecord, field: &str) -> i64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected {field} integer, got {other:?} in {row:?}"),
    }
}

fn spawn_http_server() -> String {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
}

fn post_query(addr: &str, query: &str) -> JsonValue {
    let body = json!({ "query": query }).to_string();
    let request = format!(
        "POST /query HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

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
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 for query {query:?}, got:\n{response}"
    );
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response has body");
    let parsed: JsonValue = serde_json::from_str(body).expect("json body");
    assert_eq!(parsed.get("ok").and_then(JsonValue::as_bool), Some(true));
    parsed
}

fn records(response: &JsonValue) -> &[JsonValue] {
    response["result"]["records"]
        .as_array()
        .expect("result.records array")
}

fn json_value<'a>(record: &'a JsonValue, column: &str) -> &'a JsonValue {
    &record["values"][column]
}

#[test]
fn runtime_queries_document_fields_json_paths_arrays_and_groups() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT events");
    exec(
        &rt,
        r#"INSERT INTO events DOCUMENT (body) VALUES
        ('{"level":"error","service":{"name":"checkout","tier":"edge"},"tags":["checkout","payment"],"latency_ms":42}'),
        ('{"level":"info","service":{"name":"checkout","tier":"edge"},"tags":["search"],"latency_ms":7}'),
        ('{"level":"error","service":{"name":"billing","tier":"core"},"tags":["payment"],"latency_ms":99}')"#,
    );

    let projected = exec(
        &rt,
        "SELECT body.level AS severity, json_extract(body, '$.service.name') AS service \
         FROM events \
         WHERE body.service.tier = 'edge' AND body.tags CONTAINS 'checkout' \
         ORDER BY body.latency_ms",
    );
    assert_eq!(projected.result.records.len(), 1);
    assert_eq!(text(&projected.result.records[0], "severity"), "error");
    assert_eq!(
        text(&projected.result.records[0], "service"),
        "\"checkout\""
    );

    let grouped = exec(
        &rt,
        "SELECT body.level AS severity, COUNT(*) AS count \
         FROM events \
         WHERE body.missing = 'no-match' OR body.tags CONTAINS 'payment' \
         GROUP BY body.level \
         ORDER BY body.level",
    );
    assert_eq!(grouped.result.records.len(), 1);
    assert_eq!(text(&grouped.result.records[0], "severity"), "error");
    assert_eq!(int(&grouped.result.records[0], "count"), 2);
}

#[test]
fn http_query_document_fields_json_paths_arrays_and_groups() {
    let addr = spawn_http_server();
    post_query(&addr, "CREATE DOCUMENT events");
    post_query(
        &addr,
        r#"INSERT INTO events DOCUMENT (body) VALUES
        ('{"level":"error","service":{"name":"checkout","tier":"edge"},"tags":["checkout","payment"],"latency_ms":42}'),
        ('{"level":"info","service":{"name":"checkout","tier":"edge"},"tags":["search"],"latency_ms":7}'),
        ('{"level":"error","service":{"name":"billing","tier":"core"},"tags":["payment"],"latency_ms":99}')"#,
    );

    let projected = post_query(
        &addr,
        "SELECT body.level AS severity, json_extract(body, '$.service.name') AS service \
         FROM events \
         WHERE body.service.tier = 'edge' AND body.tags CONTAINS 'checkout' \
         ORDER BY body.latency_ms",
    );
    let projected_rows = records(&projected);
    assert_eq!(projected_rows.len(), 1, "projected={projected}");
    assert_eq!(json_value(&projected_rows[0], "severity"), &json!("error"));
    assert_eq!(
        json_value(&projected_rows[0], "service"),
        &json!("\"checkout\"")
    );

    let grouped = post_query(
        &addr,
        "SELECT body.level AS severity, COUNT(*) AS count \
         FROM events \
         WHERE body.missing = 'no-match' OR body.tags CONTAINS 'payment' \
         GROUP BY body.level \
         ORDER BY body.level",
    );
    let grouped_rows = records(&grouped);
    assert_eq!(grouped_rows.len(), 1, "grouped={grouped}");
    assert_eq!(json_value(&grouped_rows[0], "severity"), &json!("error"));
    assert_eq!(json_value(&grouped_rows[0], "count"), &json!(2));
}
