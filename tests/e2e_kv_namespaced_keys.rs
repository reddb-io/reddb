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

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

#[test]
fn sql_and_dsl_preserve_quoted_namespaced_kv_keys() {
    let rt = runtime();

    exec(
        &rt,
        "INSERT INTO settings KV (key, value) VALUES ('characters:hansel', 'trail')",
    );
    let sql_read = exec(
        &rt,
        "SELECT key, value FROM settings WHERE key = 'characters:hansel'",
    );
    let sql_row = only_record(&sql_read);
    assert_eq!(text(sql_row, "key"), "characters:hansel");
    assert_eq!(text(sql_row, "value"), "trail");

    let dsl_get = exec(&rt, "KV GET settings.'characters:hansel'");
    assert_eq!(text(only_record(&dsl_get), "key"), "characters:hansel");

    let delete = exec(&rt, "KV DELETE settings.'characters:hansel'");
    assert_eq!(delete.affected_rows, 1);
    let missing = exec(
        &rt,
        "SELECT key, value FROM settings WHERE key = 'characters:hansel'",
    );
    assert_eq!(missing.result.records.len(), 0);

    exec(&rt, "KV PUT 'characters:hansel' = 'crumbs'");
    let default_get = exec(&rt, "KV GET 'characters:hansel'");
    assert_eq!(text(only_record(&default_get), "collection"), "kv_default");
    assert_eq!(text(only_record(&default_get), "key"), "characters:hansel");
    assert_eq!(text(only_record(&default_get), "value"), "crumbs");

    let default_delete = exec(&rt, "KV DELETE 'characters:hansel'");
    assert_eq!(default_delete.affected_rows, 1);
}

#[test]
fn unquoted_namespaced_kv_dsl_key_suggests_quoting() {
    let rt = runtime();

    let err = rt
        .execute_query("KV GET characters:hansel")
        .expect_err("unquoted colon key should fail");
    let message = err.to_string();
    assert!(message.contains("quote"), "unexpected error: {message}");
    assert!(
        message.contains("'characters:hansel'"),
        "unexpected error: {message}"
    );
}

#[test]
fn namespaced_kv_keys_survive_reopen() {
    let path = PersistentDbPath::new("kv_namespaced_keys");
    let rt = path.open_runtime();

    exec(&rt, "KV PUT settings.'characters:hansel' = 'trail'");
    let reopened = checkpoint_and_reopen(&path, rt);
    let read = exec(&reopened, "KV GET settings.'characters:hansel'");
    assert_eq!(text(only_record(&read), "collection"), "settings");
    assert_eq!(text(only_record(&read), "key"), "characters:hansel");
    assert_eq!(text(only_record(&read), "value"), "trail");
}

#[test]
fn http_kv_endpoints_accept_url_encoded_namespaced_keys() {
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

    let delete = http_json(
        addr,
        "DELETE",
        "/collections/settings/kvs/characters%3Ahansel",
        None,
    );
    assert_eq!(delete["deleted"], json!(true), "{delete}");

    let v1_put = http_json(
        addr,
        "PUT",
        "/v1/kv/settings/characters%3Ahansel",
        Some(json!({"value":"trail"})),
    );
    assert_eq!(v1_put["ok"].as_bool(), Some(true), "{v1_put}");

    let v1_get = http_json(addr, "GET", "/v1/kv/settings/characters%3Ahansel", None);
    assert_eq!(v1_get["collection"], json!("settings"));
    assert_eq!(v1_get["key"], json!("characters:hansel"));
    assert_eq!(v1_get["value"], json!("trail"));

    let v1_delete = http_json(addr, "DELETE", "/v1/kv/settings/characters%3Ahansel", None);
    assert_eq!(v1_delete["deleted"], json!(true), "{v1_delete}");
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
