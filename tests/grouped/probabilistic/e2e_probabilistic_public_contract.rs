#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use serde_json::{json, Value as JsonValue};
use support::{checkpoint_and_reopen, PersistentDbPath};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`",
        result.query
    );
    &result.result.records[0]
}

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?}"),
    }
}

fn bool_value(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column {column}, got {other:?}"),
    }
}

fn spawn_http_server() -> String {
    let server = RedDBServer::new(runtime());
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

fn http_only_record(response: &JsonValue) -> &JsonValue {
    let records = response["result"]["records"]
        .as_array()
        .expect("result.records array");
    assert_eq!(records.len(), 1, "expected one record in {response}");
    &records[0]
}

fn http_value<'a>(record: &'a JsonValue, column: &str) -> &'a JsonValue {
    &record["values"][column]
}

#[test]
fn probabilistic_sql_read_forms_have_default_columns_and_star_guidance() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice'");
    let hll = exec(&rt, "SELECT CARDINALITY FROM visitors");
    assert_eq!(hll.result.columns, vec!["cardinality"]);
    assert_eq!(uint_value(only_record(&hll), "cardinality"), 2);

    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 5");
    let sketch = exec(&rt, "SELECT FREQ('signup') FROM clicks");
    assert_eq!(sketch.result.columns, vec!["freq"]);
    assert_eq!(uint_value(only_record(&sketch), "freq"), 5);

    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");
    let filter = exec(&rt, "SELECT CONTAINS('sess:abc') FROM sessions");
    assert_eq!(filter.result.columns, vec!["contains"]);
    assert!(bool_value(only_record(&filter), "contains"));

    let star = rt
        .execute_query("SELECT * FROM visitors")
        .expect_err("SELECT * from a probabilistic collection should guide callers");
    let message = format!("{star:?}");
    assert!(
        message.contains("SELECT CARDINALITY, FREQ(...), or CONTAINS(...) read forms"),
        "unexpected error: {message}"
    );
}

#[test]
fn probabilistic_state_survives_reopen_for_commands_and_sql_read_forms() {
    let path = PersistentDbPath::new("probabilistic_public_contract");
    let rt = path.open_runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice'");

    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 5");

    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");

    let reopened = checkpoint_and_reopen(&path, rt);

    let hll_command = exec(&reopened, "HLL COUNT visitors");
    assert_eq!(uint_value(only_record(&hll_command), "count"), 2);
    let hll_sql = exec(
        &reopened,
        "SELECT CARDINALITY AS unique_count FROM visitors",
    );
    assert_eq!(hll_sql.result.columns, vec!["unique_count"]);
    assert_eq!(uint_value(only_record(&hll_sql), "unique_count"), 2);

    let sketch_command = exec(&reopened, "SKETCH COUNT clicks 'signup'");
    assert_eq!(uint_value(only_record(&sketch_command), "estimate"), 5);
    let sketch_sql = exec(&reopened, "SELECT FREQ('signup') AS freq FROM clicks");
    assert_eq!(sketch_sql.result.columns, vec!["freq"]);
    assert_eq!(uint_value(only_record(&sketch_sql), "freq"), 5);

    let filter_command = exec(&reopened, "FILTER CHECK sessions 'sess:abc'");
    assert!(bool_value(only_record(&filter_command), "exists"));
    let filter_sql = exec(
        &reopened,
        "SELECT CONTAINS('sess:abc') AS hit FROM sessions",
    );
    assert_eq!(filter_sql.result.columns, vec!["hit"]);
    assert!(bool_value(only_record(&filter_sql), "hit"));
}

#[test]
fn http_query_covers_probabilistic_commands_and_sql_read_forms() {
    let addr = spawn_http_server();

    post_query(&addr, "CREATE HLL visitors");
    post_query(&addr, "HLL ADD visitors 'alice' 'bob' 'alice'");
    let hll_command = post_query(&addr, "HLL COUNT visitors");
    assert_eq!(
        http_value(http_only_record(&hll_command), "count"),
        &json!(2)
    );
    let hll_sql = post_query(&addr, "SELECT CARDINALITY AS unique_count FROM visitors");
    assert_eq!(
        http_value(http_only_record(&hll_sql), "unique_count"),
        &json!(2)
    );

    post_query(&addr, "CREATE SKETCH clicks");
    post_query(&addr, "SKETCH ADD clicks 'signup' 5");
    let sketch_command = post_query(&addr, "SKETCH COUNT clicks 'signup'");
    assert_eq!(
        http_value(http_only_record(&sketch_command), "estimate"),
        &json!(5)
    );
    let sketch_sql = post_query(&addr, "SELECT FREQ('signup') AS freq FROM clicks");
    assert_eq!(http_value(http_only_record(&sketch_sql), "freq"), &json!(5));

    post_query(&addr, "CREATE FILTER sessions");
    post_query(&addr, "FILTER ADD sessions 'sess:abc'");
    let filter_command = post_query(&addr, "FILTER CHECK sessions 'sess:abc'");
    assert_eq!(
        http_value(http_only_record(&filter_command), "exists"),
        &json!(true)
    );
    let filter_sql = post_query(&addr, "SELECT CONTAINS('sess:abc') AS hit FROM sessions");
    assert_eq!(
        http_value(http_only_record(&filter_sql), "hit"),
        &json!(true)
    );
}
