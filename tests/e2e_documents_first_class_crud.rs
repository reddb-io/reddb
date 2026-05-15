mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::application::{
    CreateDocumentInput, ExecuteQueryInput, PatchEntityInput, PatchEntityOperation,
    PatchEntityOperationType,
};
use reddb::json::Value as RedJsonValue;
use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use reddb::{EntityUseCases, QueryUseCases};
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

fn json_field<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> JsonValue {
    match row.get(field) {
        Some(Value::Json(value)) => serde_json::from_slice(value)
            .unwrap_or_else(|err| panic!("expected {field} JSON to decode: {err}")),
        other => panic!("expected {field} JSON, got {other:?} in {row:?}"),
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
        inserted.result.records[0].get("rid").is_some(),
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

    let patch = json!({
        "operations": [
            { "op": "set", "path": "/body/details/reviewed", "value": true },
            { "op": "set", "path": "/body/preferences/notifications/email", "value": "weekly" },
            { "op": "unset", "path": "/body/details/agent" },
            { "op": "unset", "path": "/body/details/missing" }
        ]
    });
    let (status, patched) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/events/entities/{id}"),
        Some(patch),
    );
    assert_eq!(status, 200, "patched={patched}");
    assert_eq!(
        patched["entity"]["data"]["named"]["body"]["details"]["reviewed"],
        json!(true)
    );
    assert_eq!(
        patched["entity"]["data"]["named"]["body"]["preferences"]["notifications"]["email"],
        json!("weekly")
    );
    assert!(
        patched["entity"]["data"]["named"]["body"]["details"]
            .get("agent")
            .is_none(),
        "unset should remove nested agent: {patched}"
    );

    let array_patch = json!({
        "operations": [
            { "op": "set", "path": "/body/roles/0", "value": "owner" }
        ]
    });
    let (status, array_error) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/events/entities/{id}"),
        Some(array_patch),
    );
    assert_eq!(status, 400, "array_error={array_error}");
    assert!(
        array_error["error"]
            .as_str()
            .is_some_and(|message| message.contains("array positional document patch paths")),
        "array positional error should be helpful: {array_error}"
    );

    let replacement = json!({
        "body": {
            "event_type": "replaced",
            "details": { "ip": "127.0.0.1" }
        }
    });
    let (status, replaced) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/events/entities/{id}"),
        Some(replacement),
    );
    assert_eq!(status, 200, "replaced={replaced}");
    assert_eq!(
        replaced["entity"]["data"]["named"]["body"]["event_type"],
        json!("replaced")
    );
    assert!(
        replaced["entity"]["data"]["named"]["body"]
            .get("roles")
            .is_none(),
        "full body replacement should remove old document fields: {replaced}"
    );
    assert_eq!(
        replaced["entity"]["data"]["named"]["event_type"],
        json!("replaced")
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

#[test]
fn runtime_document_patch_updates_nested_body_and_survives_reopen() {
    let path = PersistentDbPath::new("document_patch_runtime");
    let rt = path.open_runtime();
    let entity = EntityUseCases::new(&rt);

    rt.execute_query("CREATE DOCUMENT profiles")
        .expect("CREATE DOCUMENT should succeed");
    let created = entity
        .create_document(CreateDocumentInput {
            collection: "profiles".into(),
            body: reddb::json::from_str(
                r#"{
                    "name": "Ada",
                    "contact": { "email": "ada@example.test", "phone": "555-0100" },
                    "settings": { "theme": "dark" }
                }"#,
            )
            .expect("document body JSON"),
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        })
        .expect("create document");

    let patched = entity
        .patch(PatchEntityInput {
            collection: "profiles".into(),
            id: created.id,
            payload: RedJsonValue::Object(Default::default()),
            operations: vec![
                PatchEntityOperation {
                    op: PatchEntityOperationType::Set,
                    path: vec!["body".into(), "contact".into(), "verified".into()],
                    value: Some(RedJsonValue::Bool(true)),
                },
                PatchEntityOperation {
                    op: PatchEntityOperationType::Set,
                    path: vec![
                        "body".into(),
                        "preferences".into(),
                        "notifications".into(),
                        "email".into(),
                    ],
                    value: Some(RedJsonValue::String("weekly".into())),
                },
                PatchEntityOperation {
                    op: PatchEntityOperationType::Unset,
                    path: vec!["body".into(), "contact".into(), "phone".into()],
                    value: None,
                },
                PatchEntityOperation {
                    op: PatchEntityOperationType::Unset,
                    path: vec!["body".into(), "contact".into(), "missing".into()],
                    value: None,
                },
            ],
        })
        .expect("patch document body");
    let patched_body = patched
        .entity
        .as_ref()
        .and_then(|entity| match &entity.data {
            reddb::storage::EntityData::Row(row) => row.named.as_ref(),
            _ => None,
        })
        .and_then(|named| named.get("body"))
        .and_then(|value| match value {
            Value::Json(bytes) => serde_json::from_slice::<JsonValue>(bytes).ok(),
            _ => None,
        })
        .expect("patched document body");
    assert_eq!(patched_body["contact"]["verified"], json!(true));
    assert_eq!(
        patched_body["preferences"]["notifications"]["email"],
        json!("weekly")
    );
    assert!(patched_body["contact"].get("phone").is_none());

    let reopened = checkpoint_and_reopen(&path, rt);
    let query = QueryUseCases::new(&reopened);
    let after = query
        .execute(ExecuteQueryInput {
            query: "SELECT name, body FROM profiles WHERE name = 'Ada'".into(),
        })
        .expect("select patched document");
    assert_eq!(after.result.records.len(), 1);
    let body = json_field(&after.result.records[0], "body");
    assert_eq!(body["contact"]["verified"], json!(true));
    assert_eq!(
        body["preferences"]["notifications"]["email"],
        json!("weekly")
    );
    assert!(body["contact"].get("phone").is_none());
}
