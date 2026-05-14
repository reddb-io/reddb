mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};
use support::{checkpoint_and_reopen, PersistentDbPath};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn text_field<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn number_field(row: &reddb::storage::query::UnifiedRecord, field: &str) -> f64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value as f64,
        Some(Value::Float(value)) => *value,
        other => panic!("expected {field} number, got {other:?} in {row:?}"),
    }
}

fn spawn_http_server() -> String {
    let server = RedDBServer::new(runtime());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
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

#[test]
fn create_document_insert_returning_select_and_reopen() {
    let path = PersistentDbPath::new("documents_first_class");
    let rt = path.open_runtime();

    rt.execute_query("CREATE DOCUMENT events")
        .expect("CREATE DOCUMENT should succeed");
    let catalog = rt
        .execute_query("SELECT name, model FROM red.collections WHERE name = 'events'")
        .expect("document collection should be cataloged");
    assert_eq!(catalog.result.records.len(), 1);
    assert_eq!(text_field(&catalog.result.records[0], "model"), "document");

    let inserted = rt
        .execute_query(
            r#"INSERT INTO events DOCUMENT (body) VALUES ('{"event_type":"login","success":true,"attempts":2,"details":{"ip":"10.0.0.5","agent":"cli"},"roles":["admin","ops"]}') RETURNING *"#,
        )
        .expect("document insert returning should succeed");
    assert_eq!(inserted.affected_rows, 1);
    assert_eq!(inserted.result.records.len(), 1);
    assert!(
        inserted.result.records[0].get("red_entity_id").is_some(),
        "RETURNING * should expose stable document identity"
    );
    assert_eq!(
        text_field(&inserted.result.records[0], "event_type"),
        "login"
    );
    assert_eq!(number_field(&inserted.result.records[0], "attempts"), 2.0);

    let selected = rt
        .execute_query("SELECT event_type, attempts, body FROM events WHERE event_type = 'login'")
        .expect("document rows should be SQL-queryable");
    assert_eq!(selected.result.records.len(), 1);
    assert_eq!(
        text_field(&selected.result.records[0], "event_type"),
        "login"
    );
    assert!(matches!(
        selected.result.records[0].get("body"),
        Some(Value::Json(_))
    ));

    let reopened = checkpoint_and_reopen(&path, rt);
    let after = reopened
        .execute_query("SELECT event_type, attempts, body FROM events WHERE event_type = 'login'")
        .expect("document rows should survive reopen");
    assert_eq!(after.result.records.len(), 1);
    assert_eq!(text_field(&after.result.records[0], "event_type"), "login");
    assert_eq!(number_field(&after.result.records[0], "attempts"), 2.0);
}

#[test]
fn http_document_insert_get_scan_and_delete() {
    let addr = spawn_http_server();

    let payload = json!({
        "body": {
            "event_type": "logout",
            "success": false,
            "details": { "ip": "10.0.0.6", "agent": "browser" },
            "roles": ["reader", "ops"]
        }
    });
    let (status, created) = http_request(
        &addr,
        "POST",
        "/collections/events/documents",
        Some(payload),
    );
    assert_eq!(status, 200, "created={created}");
    assert_eq!(created["ok"].as_bool(), Some(true));
    let id = created["id"].as_u64().expect("created id");

    let (status, fetched) = http_request(
        &addr,
        "GET",
        &format!("/collections/events/entities/{id}"),
        None,
    );
    assert_eq!(status, 200, "fetched={fetched}");
    assert_eq!(fetched["data"]["named"]["event_type"], json!("logout"));
    assert_eq!(
        fetched["data"]["named"]["body"]["details"]["ip"],
        json!("10.0.0.6")
    );

    let (status, scanned) = http_request(
        &addr,
        "GET",
        "/collections/events/scan?offset=0&limit=10",
        None,
    );
    assert_eq!(status, 200, "scanned={scanned}");
    assert_eq!(scanned["total"].as_u64(), Some(1));
    assert_eq!(
        scanned["items"][0]["data"]["named"]["body"]["roles"][1],
        json!("ops")
    );

    let (status, deleted) = http_request(
        &addr,
        "DELETE",
        &format!("/collections/events/entities/{id}"),
        None,
    );
    assert_eq!(status, 200, "deleted={deleted}");
    assert_eq!(deleted["deleted"].as_bool(), Some(true));

    let (status, after_delete) = http_request(
        &addr,
        "GET",
        &format!("/collections/events/entities/{id}"),
        None,
    );
    assert_eq!(status, 404, "after_delete={after_delete}");
}
