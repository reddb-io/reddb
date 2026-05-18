// Regression coverage for issue #541 — KV namespaced keys (`:`).
//
// The implementation behind every acceptance bullet shipped under #456
// (commit `fc2bdc15`); this file pins those bullets down with a
// dedicated regression suite traceable to #541 so future breakage is
// localised. One test per acceptance bullet in the AGENT-BRIEF.

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use serde_json::{json, Value as JsonValue};

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

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

// Acceptance: "Quoted namespaced key works through SQL GET/SET KV".
#[test]
fn quoted_namespaced_key_round_trips_through_sql_set_and_get() {
    let rt = runtime();

    exec(&rt, "KV PUT settings.'characters:hansel' = 'trail'");
    let read = exec(&rt, "KV GET settings.'characters:hansel'");
    let row = only_record(&read);
    assert_eq!(text(row, "collection"), "settings");
    assert_eq!(text(row, "key"), "characters:hansel");
    assert_eq!(text(row, "value"), "trail");

    let sql_read = exec(
        &rt,
        "SELECT key, value FROM settings WHERE key = 'characters:hansel'",
    );
    let sql_row = only_record(&sql_read);
    assert_eq!(text(sql_row, "key"), "characters:hansel");
    assert_eq!(text(sql_row, "value"), "trail");
}

// Acceptance: "URL-encoded namespaced key works through HTTP KV
// endpoints". Covers both the `/collections/<name>/kvs/<key>` and the
// `/v1/kv/<collection>/<key>` surfaces — both are publicly documented.
#[test]
fn url_encoded_namespaced_key_round_trips_through_http_kv_endpoints() {
    let server = RedDBServer::new(runtime());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    server.serve_in_background_on(listener);

    let put = http_json(
        addr,
        "PUT",
        "/collections/settings/kvs/characters%3Ahansel",
        Some(json!({"value":"trail"})),
    );
    assert_eq!(put["ok"].as_bool(), Some(true), "{put}");

    let get = http_json(
        addr,
        "GET",
        "/collections/settings/kvs/characters%3Ahansel",
        None,
    );
    assert_eq!(get["collection"], json!("settings"));
    assert_eq!(get["key"], json!("characters:hansel"));
    assert_eq!(get["value"], json!("trail"));

    let v1_put = http_json(
        addr,
        "PUT",
        "/v1/kv/settings/characters%3Ahansel",
        Some(json!({"value":"trail"})),
    );
    assert_eq!(v1_put["ok"].as_bool(), Some(true), "{v1_put}");

    let v1_get = http_json(addr, "GET", "/v1/kv/settings/characters%3Ahansel", None);
    assert_eq!(v1_get["key"], json!("characters:hansel"));
    assert_eq!(v1_get["value"], json!("trail"));
}

// Acceptance: "SDK KV helpers accept the raw string with `:` (no
// escaping required)". The JS SDK helpers live outside this Rust suite
// (covered by `drivers/js-client/test/kv.test.mjs` and
// `drivers/js/test/kv.test.mjs`); the contract the SDK relies on is
// that the *server* accepts the SQL the helpers generate — i.e. a
// single-quoted key segment after `<collection>.`. This test pins that
// server-side contract so an SDK regression caused by a server-side
// drift surfaces here even if no SDK test runs.
#[test]
fn server_accepts_sdk_generated_sql_for_namespaced_keys() {
    let rt = runtime();

    // Shape of SQL emitted by `KvClient.put` / `.get` for a key that
    // matches `/[^A-Za-z0-9_]/` (e.g. `corpus:version`).
    exec(&rt, "KV PUT kv_default.'corpus:version' = '1.0.0'");
    let get = exec(&rt, "KV GET kv_default.'corpus:version'");
    assert_eq!(text(only_record(&get), "value"), "1.0.0");
}

// Acceptance: "Error message audit: bad unquoted namespaced key in SQL
// produces helpful guidance".
#[test]
fn unquoted_namespaced_key_in_sql_suggests_quoting_with_the_offending_key() {
    let rt = runtime();

    let err = rt
        .execute_query("KV GET characters:hansel")
        .expect_err("unquoted colon key should fail");
    let message = err.to_string();
    assert!(
        message.contains("quote"),
        "expected guidance to mention quoting; got: {message}"
    );
    assert!(
        message.contains("'characters:hansel'"),
        "expected guidance to echo the quoted key form; got: {message}"
    );
}

fn http_json(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<JsonValue>,
) -> JsonValue {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
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
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 200")
            || response.starts_with("HTTP/1.1 201")
            || response.starts_with("HTTP/1.1 204"),
        "expected success for {method} {path}, got:\n{response}"
    );
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response body");
    serde_json::from_str(body).expect("json body")
}
