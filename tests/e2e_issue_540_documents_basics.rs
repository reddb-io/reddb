// Regression coverage for issue #540 — Documents: CREATE DOCUMENT DDL +
// insert (SQL/HTTP) + GET by id + persistence.
//
// Each test in this file maps to one bullet in the issue's `## Acceptance`
// list, so a future regression is traceable back to the specific public
// promise it broke.

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};
use support::{checkpoint_and_reopen, PersistentDbPath};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn text_field<'a>(row: &'a UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text {field}, got {other:?} in {row:?}"),
    }
}

fn number_field(row: &UnifiedRecord, field: &str) -> f64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value as f64,
        Some(Value::Float(value)) => *value,
        other => panic!("expected number {field}, got {other:?} in {row:?}"),
    }
}

fn rid_field(row: &UnifiedRecord) -> u64 {
    match row.get("rid") {
        Some(Value::Integer(value)) => *value as u64,
        Some(Value::UnsignedInteger(value)) => *value,
        other => panic!("expected rid integer, got {other:?} in {row:?}"),
    }
}

fn spawn_http_server() -> (RedDBRuntime, String) {
    let rt = runtime();
    let server = RedDBServer::new(rt.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr").to_string();
    server.serve_in_background_on(listener);
    (rt, addr)
}

fn http_request(addr: &str, method: &str, path: &str, body: Option<JsonValue>) -> (u16, JsonValue) {
    let body_text = body.map(|body| body.to_string());
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(body_text) = &body_text {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body_text.len(),
            body_text
        ));
    } else {
        request.push_str("\r\n");
    }

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
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let parsed = serde_json::from_str(body).unwrap_or_else(|_| json!({ "raw": body }));
    (status, parsed)
}

// Acceptance: CREATE DOCUMENT <name> DDL parses, runs, and registers a
// document-model `Collection`.
#[test]
fn create_document_ddl_registers_document_model_collection() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue540_events")
        .expect("CREATE DOCUMENT should parse and run");

    let catalog = rt
        .execute_query("SELECT name, model FROM red.collections WHERE name = 'issue540_events'")
        .expect("catalog query should succeed");
    assert_eq!(catalog.result.records.len(), 1);
    assert_eq!(
        text_field(&catalog.result.records[0], "model"),
        "document",
        "CREATE DOCUMENT must register a document-model collection",
    );
}

// Acceptance: SQL INSERT stores a JSON document.
#[test]
fn sql_insert_stores_json_document_and_get_by_id_returns_it() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue540_sql_events")
        .expect("CREATE DOCUMENT");

    let inserted = rt
        .execute_query(
            r#"INSERT INTO issue540_sql_events DOCUMENT (body)
               VALUES ('{"event_type":"login","attempts":2,"meta":{"ip":"10.0.0.1"}}')
               RETURNING *"#,
        )
        .expect("SQL insert should succeed");
    assert_eq!(inserted.affected_rows, 1);
    let inserted_record = &inserted.result.records[0];
    let rid = rid_field(inserted_record);
    assert_eq!(text_field(inserted_record, "event_type"), "login");
    assert_eq!(number_field(inserted_record, "attempts"), 2.0);

    // GET by id via SQL — WHERE rid = <id> is the documented document
    // by-id lookup form alongside the HTTP /entities/{id} surface.
    let by_id = rt
        .execute_query(&format!(
            "SELECT event_type, attempts FROM issue540_sql_events WHERE rid = {rid}"
        ))
        .expect("SQL GET by id should succeed");
    assert_eq!(by_id.result.records.len(), 1);
    assert_eq!(text_field(&by_id.result.records[0], "event_type"), "login");
    assert_eq!(number_field(&by_id.result.records[0], "attempts"), 2.0);
}

// Acceptance: HTTP POST stores a JSON document and HTTP GET by id returns it.
#[test]
fn http_post_stores_document_and_http_get_by_id_returns_it() {
    let (_rt, addr) = spawn_http_server();
    let (status, _) = http_request(
        &addr,
        "POST",
        "/query",
        Some(json!({ "query": "CREATE DOCUMENT issue540_http_events" })),
    );
    assert_eq!(status, 200);

    let (status, created) = http_request(
        &addr,
        "POST",
        "/collections/issue540_http_events/documents",
        Some(json!({
            "body": {
                "event_type": "logout",
                "attempts": 1,
                "meta": { "ip": "10.0.0.2" }
            }
        })),
    );
    assert_eq!(status, 200, "created={created}");
    assert_eq!(created["ok"].as_bool(), Some(true));
    let id = created["id"].as_u64().expect("created id");

    let (status, fetched) = http_request(
        &addr,
        "GET",
        &format!("/collections/issue540_http_events/entities/{id}"),
        None,
    );
    assert_eq!(status, 200, "fetched={fetched}");
    assert_eq!(fetched["data"]["named"]["event_type"], json!("logout"));
    assert_eq!(
        fetched["data"]["named"]["body"]["meta"]["ip"],
        json!("10.0.0.2")
    );
}

// Acceptance: document persists across an engine reopen.
#[test]
fn document_survives_engine_reopen() {
    let path = PersistentDbPath::new("issue540_reopen");
    let rt = path.open_runtime();
    rt.execute_query("CREATE DOCUMENT issue540_persist")
        .expect("CREATE DOCUMENT");
    rt.execute_query(
        r#"INSERT INTO issue540_persist DOCUMENT (body)
           VALUES ('{"event_type":"durable","attempts":3}')"#,
    )
    .expect("INSERT");

    let reopened = checkpoint_and_reopen(&path, rt);

    let after = reopened
        .execute_query(
            "SELECT event_type, attempts FROM issue540_persist WHERE event_type = 'durable'",
        )
        .expect("post-reopen query");
    assert_eq!(after.result.records.len(), 1);
    assert_eq!(
        text_field(&after.result.records[0], "event_type"),
        "durable"
    );
    assert_eq!(number_field(&after.result.records[0], "attempts"), 3.0);
}

// Acceptance: README example for documents has at least one automated test
// backing it. The README at the repo root shows:
//
//   INSERT INTO logs DOCUMENT (body) VALUES ('{"level":"info","msg":"login"}')
//
// This test exercises that exact statement end-to-end.
#[test]
fn readme_documents_example_runs_end_to_end() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT logs")
        .expect("CREATE DOCUMENT logs");
    let inserted = rt
        .execute_query(
            r#"INSERT INTO logs DOCUMENT (body)
               VALUES ('{"level":"info","msg":"login"}') RETURNING *"#,
        )
        .expect("README INSERT should succeed");
    assert_eq!(inserted.affected_rows, 1);

    let selected = rt
        .execute_query("SELECT level, msg FROM logs WHERE level = 'info'")
        .expect("README-style SELECT should succeed");
    assert_eq!(selected.result.records.len(), 1);
    assert_eq!(text_field(&selected.result.records[0], "level"), "info");
    assert_eq!(text_field(&selected.result.records[0], "msg"), "login");
}
