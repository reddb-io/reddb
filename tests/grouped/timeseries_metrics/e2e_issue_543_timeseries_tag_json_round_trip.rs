//! Issue #543 — Time-series: tag JSON round-trip.
//!
//! Tags on a time-series point must come back out of the engine as
//! real JSON values, not stringified placeholders. The brief asks for
//! parity between the SQL surface and the HTTP `/query` envelope, and
//! a regression test covering a representative tag payload (mixed
//! scalars + nested object + array).
//!
//! Prior to this slice the runtime collapsed every tag value into a
//! `String` on the way in and re-wrapped each one as a JSON string on
//! the way out, so a payload like `{port: 8080, active: true,
//! roles: ['edge', 'cache']}` came back as
//! `{"port":"8080","active":"true","roles":"[\"edge\",\"cache\"]"}`.
//! Those quoted-number and stringified-array shapes are exactly the
//! "placeholder strings" user-story 26 calls out as broken.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::json::Value as JsonValue;
use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::server::RedDBServer;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use serde_json::{json, Value as SerdeValue};

const SETUP_SQL: [&str; 2] = [
    "CREATE TIMESERIES host_metrics RETENTION 7 d",
    "INSERT INTO host_metrics (metric, value, tags, timestamp) VALUES \
     ('cpu.idle', 0.75, \
      {host: 'srv1', port: 8080, active: true, missing: null, \
       roles: ['edge', 'cache'], owner: {team: 'ops', tier: 2}}, \
      1704067200000000000)",
];

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected exactly one row, got {} for `{}`",
        result.result.records.len(),
        result.query
    );
    &result.result.records[0]
}

fn tags_object(row: &UnifiedRecord) -> JsonValue {
    match row.get("tags") {
        Some(Value::Json(bytes)) => reddb::json::from_slice(bytes)
            .unwrap_or_else(|err| panic!("tags column must be valid JSON: {err}")),
        other => panic!("expected tags column as Value::Json, got {other:?}"),
    }
}

fn assert_representative_payload(tags: &JsonValue) {
    let object = match tags {
        JsonValue::Object(map) => map,
        other => panic!("tags must be a JSON object, got {other:?}"),
    };

    assert_eq!(
        object.get("host").and_then(JsonValue::as_str),
        Some("srv1"),
        "string tag values must remain strings (not double-encoded)"
    );

    let port = object.get("port").unwrap_or(&JsonValue::Null);
    assert!(
        matches!(port, JsonValue::Number(_)),
        "numeric tag value must round-trip as JSON number, got {port:?}"
    );
    assert_eq!(port.as_i64(), Some(8080));

    let active = object.get("active").unwrap_or(&JsonValue::Null);
    assert!(
        matches!(active, JsonValue::Bool(true)),
        "boolean tag value must round-trip as JSON bool, got {active:?}"
    );

    let missing = object.get("missing").unwrap_or(&JsonValue::Bool(true));
    assert!(
        matches!(missing, JsonValue::Null),
        "null tag value must round-trip as JSON null, got {missing:?}"
    );

    match object.get("roles") {
        Some(JsonValue::Array(items)) => {
            let labels: Vec<&str> = items.iter().filter_map(JsonValue::as_str).collect();
            assert_eq!(labels, vec!["edge", "cache"], "array tag preserves order");
        }
        other => panic!("array tag must round-trip as JSON array, got {other:?}"),
    }

    match object.get("owner") {
        Some(JsonValue::Object(owner)) => {
            assert_eq!(owner.get("team").and_then(JsonValue::as_str), Some("ops"));
            assert_eq!(owner.get("tier").and_then(JsonValue::as_i64), Some(2));
        }
        other => panic!("nested object tag must round-trip as JSON object, got {other:?}"),
    }
}

#[test]
fn sql_select_tags_returns_json_typed_values() {
    let rt = runtime();
    for sql in SETUP_SQL {
        exec(&rt, sql);
    }

    let result = exec(&rt, "SELECT metric, tags FROM host_metrics");
    let row = only_record(&result);
    let tags = tags_object(row);
    assert_representative_payload(&tags);
}

fn spawn_http_server() -> String {
    let rt = runtime();
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
}

fn post_query(addr: &str, query: &str) -> SerdeValue {
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
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 for query {query:?}, got:\n{response}"
    );
    let response_body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response has body");
    serde_json::from_str(response_body).expect("json body")
}

#[test]
fn http_query_tags_match_sql_shape() {
    let addr = spawn_http_server();

    for sql in SETUP_SQL {
        let _ = post_query(&addr, sql);
    }

    let response = post_query(&addr, "SELECT metric, tags FROM host_metrics");
    let records = response["result"]["records"]
        .as_array()
        .expect("result.records array");
    assert_eq!(records.len(), 1, "expected one row, got {records:?}");
    let row = &records[0];

    // HTTP exposes the per-row fields under `values`.
    let tags_value = row
        .get("values")
        .and_then(|v| v.get("tags"))
        .or_else(|| row.get("tags"))
        .unwrap_or_else(|| panic!("tags column missing from HTTP row: {row}"));

    // Re-parse through reddb::json so the type-level assertion helper
    // in this file can run against both surfaces.
    let parsed: JsonValue = reddb::json::from_str(&tags_value.to_string())
        .unwrap_or_else(|err| panic!("HTTP tags must be valid JSON: {err}"));
    assert_representative_payload(&parsed);

    let metric_value = row
        .get("values")
        .and_then(|v| v.get("metric"))
        .or_else(|| row.get("metric"))
        .unwrap_or_else(|| panic!("metric column missing: {row}"));
    assert_eq!(metric_value, &json!("cpu.idle"));
}
