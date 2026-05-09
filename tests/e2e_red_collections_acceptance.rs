//! Issue #244 acceptance tests for `red.collections`.
//!
//! These are intentionally written against public execution surfaces only.
//! They may fail on this branch until the parser/runtime workers land the
//! `red.collections` implementation and `SHOW COLLECTIONS` parser support.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::runtime::within_clause::{FieldOverride, ScopeOverride};
use reddb::server::RedDBServer;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

const COLLECTION_COLUMNS: [&str; 9] = [
    "name",
    "model",
    "schema_mode",
    "entities",
    "segments",
    "indices",
    "in_memory_bytes",
    "internal",
    "tenant_id",
];

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn seed_collection_inventory(rt: &RedDBRuntime) {
    exec(rt, "SET TENANT 'acme'");
    exec(
        rt,
        "CREATE TABLE acme_orders (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        rt,
        "CREATE INDEX acme_orders_id ON acme_orders (id) USING HASH",
    );

    exec(rt, "SET TENANT 'globex'");
    exec(
        rt,
        "CREATE TABLE globex_orders (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );

    exec(rt, "SET TENANT NULL");
}

fn assert_collection_columns(columns: &[String]) {
    let expected = COLLECTION_COLUMNS.map(str::to_string).to_vec();
    assert_eq!(
        columns, expected,
        "red.collections should expose only the accepted system columns"
    );
}

fn text_field<'a>(record: &'a UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn bool_field(record: &UnifiedRecord, field: &str) -> bool {
    match record.get(field) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected {field} bool field, got {other:?} in {record:?}"),
    }
}

fn names(records: &[UnifiedRecord]) -> Vec<String> {
    let mut out: Vec<String> = records
        .iter()
        .map(|record| text_field(record, "name").to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn select_star_from_red_collections_returns_collection_inventory() {
    let rt = open_runtime();
    seed_collection_inventory(&rt);

    let result = rt
        .execute_query("SELECT * FROM red.collections")
        .expect("SELECT * FROM red.collections should execute");

    assert_collection_columns(&result.result.columns);
    let names = names(&result.result.records);
    assert!(
        names.iter().any(|name| name == "acme_orders"),
        "expected acme_orders in {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "globex_orders"),
        "expected globex_orders in {names:?}"
    );

    let acme = result
        .result
        .records
        .iter()
        .find(|record| text_field(record, "name") == "acme_orders")
        .expect("acme_orders row");
    assert_eq!(text_field(acme, "tenant_id"), "acme");
    assert!(
        matches!(acme.get("model"), Some(Value::Text(_))),
        "model should be present: {acme:?}"
    );
    assert!(
        matches!(acme.get("schema_mode"), Some(Value::Text(_))),
        "schema_mode should be present: {acme:?}"
    );
    for numeric in ["entities", "segments", "in_memory_bytes"] {
        assert!(
            matches!(
                acme.get(numeric),
                Some(Value::Integer(_)) | Some(Value::UnsignedInteger(_))
            ),
            "{numeric} should be an integer metric: {acme:?}"
        );
    }
    assert!(
        matches!(acme.get("indices"), Some(Value::Array(_))),
        "indices should be an array metric: {acme:?}"
    );
    assert_eq!(bool_field(acme, "internal"), false);
}

#[test]
fn show_collections_reaches_same_result_as_selecting_red_collections() {
    let rt = open_runtime();
    seed_collection_inventory(&rt);

    let via_select = rt
        .execute_query("SELECT * FROM red.collections WHERE internal = false")
        .expect("filtered SELECT * FROM red.collections should execute");
    let via_show = rt
        .execute_query("SHOW COLLECTIONS")
        .expect("SHOW COLLECTIONS should execute");

    assert_collection_columns(&via_show.result.columns);
    assert_eq!(via_show.result.columns, via_select.result.columns);
    assert_eq!(
        names(&via_show.result.records),
        names(&via_select.result.records)
    );
}

#[test]
fn dlq_is_internal_and_show_collections_hides_it_by_default() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE foo WITH DLQ failed_foo");

    let catalog = rt
        .execute_query("SELECT name, internal FROM red.collections WHERE name = 'failed_foo'")
        .expect("red.collections should include DLQ metadata");
    assert_eq!(catalog.result.records.len(), 1);
    assert_eq!(bool_field(&catalog.result.records[0], "internal"), true);

    let shown = rt
        .execute_query("SHOW COLLECTIONS")
        .expect("SHOW COLLECTIONS should execute");
    let shown_names = names(&shown.result.records);
    assert!(shown_names.iter().any(|name| name == "foo"));
    assert!(
        !shown_names.iter().any(|name| name == "failed_foo"),
        "default SHOW COLLECTIONS should hide DLQs: {shown_names:?}"
    );
}

#[test]
fn show_collections_including_internal_returns_dlq() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE foo WITH DLQ failed_foo");

    let shown = rt
        .execute_query("SHOW COLLECTIONS INCLUDING INTERNAL")
        .expect("SHOW COLLECTIONS INCLUDING INTERNAL should execute");
    let shown_names = names(&shown.result.records);
    assert!(shown_names.iter().any(|name| name == "foo"));
    assert!(
        shown_names.iter().any(|name| name == "failed_foo"),
        "INCLUDING INTERNAL should reveal DLQs: {shown_names:?}"
    );
}

#[test]
fn red_collections_respects_tenant_scope_and_cluster_admin_bypass() {
    let rt = open_runtime();
    seed_collection_inventory(&rt);

    let acme = ScopeOverride {
        tenant: FieldOverride::Set("acme".into()),
        ..Default::default()
    };
    let acme_result = rt
        .execute_query_with_scope("SELECT * FROM red.collections", acme)
        .expect("tenant-scoped red.collections query should execute");
    let acme_names = names(&acme_result.result.records);
    assert!(acme_names.iter().any(|name| name == "acme_orders"));
    assert!(
        !acme_names.iter().any(|name| name == "globex_orders"),
        "acme tenant must not see globex collections: {acme_names:?}"
    );

    let cluster_admin = ScopeOverride {
        tenant: FieldOverride::Clear,
        role: FieldOverride::Set("admin".into()),
        ..Default::default()
    };
    let admin_result = rt
        .execute_query_with_scope("SELECT * FROM red.collections", cluster_admin)
        .expect("cluster admin red.collections query should execute");
    let admin_names = names(&admin_result.result.records);
    assert!(admin_names.iter().any(|name| name == "acme_orders"));
    assert!(admin_names.iter().any(|name| name == "globex_orders"));
}

#[test]
fn red_system_schema_is_read_only_for_dml() {
    let rt = open_runtime();
    seed_collection_inventory(&rt);

    for sql in [
        "INSERT INTO red.collections (name) VALUES ('evil')",
        "UPDATE red.collections SET model = 'table' WHERE name = 'acme_orders'",
        "DELETE FROM red.collections WHERE name = 'acme_orders'",
    ] {
        let err = match rt.execute_query(sql) {
            Ok(result) => panic!("{sql} should fail, got {result:?}"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("system schema is read-only"),
            "{sql} should report read-only system schema, got {message:?}"
        );
    }
}

fn spawn_http(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    addr.to_string()
}

fn http_post_query(addr: &str, query: &str) -> (u16, serde_json::Value) {
    let body = serde_json::json!({ "query": query }).to_string();
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!(
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
    let json = serde_json::from_str(&resp[body_idx..])
        .unwrap_or_else(|err| panic!("HTTP body should be JSON: {err}: {resp}"));
    (status, json)
}

#[test]
fn http_query_endpoint_returns_red_collections_inventory() {
    let rt = open_runtime();
    seed_collection_inventory(&rt);
    let addr = spawn_http(rt);

    let (status, body) = http_post_query(&addr, "SELECT * FROM red.collections");

    assert_eq!(status, 200, "body = {body}");
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["result"]["columns"],
        serde_json::json!(COLLECTION_COLUMNS)
    );
    let encoded = body.to_string();
    assert!(
        encoded.contains("acme_orders") && encoded.contains("globex_orders"),
        "HTTP /query should return collection rows, got {body}"
    );
}
