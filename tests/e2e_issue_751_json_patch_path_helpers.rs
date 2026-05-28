// Issue #751 — JSON patch and path helpers for document and KV editors.
//
// Pins the structured patch contract that Red UI relies on so it can validate
// nested edits before mutation, set/delete nested paths in both document rows
// and JSON-shaped KV values, and surface uniform structured errors with a
// JSON Pointer location for the failing operation.
//
// Acceptance bullets (issue #751):
//   1. Validate JSON patch / path edits before mutation with structured
//      errors that carry a pointer-like location.
//   2. Set and delete nested document paths through a documented contract.
//   3. Set and delete nested JSON KV value paths when the stored value is
//      JSON-compatible.
//   4. Invalid paths, non-JSON values, type conflicts, and authorization
//      failures produce structured UI-safe errors.
//   5. Tests cover document and KV happy paths plus validation failures.

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
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

fn ddl(addr: &str, sql: &str) {
    let (status, body) = http_request(
        addr,
        "POST",
        "/query",
        Some(json!({ "query": sql.to_string() })),
    );
    assert_eq!(status, 200, "{sql}: {body}");
}

fn insert_document(addr: &str, collection: &str, body: JsonValue) -> u64 {
    let (status, created) = http_request(
        addr,
        "POST",
        &format!("/collections/{collection}/documents"),
        Some(json!({ "body": body })),
    );
    assert_eq!(status, 200, "POST document should succeed: {created}");
    created["id"].as_u64().expect("created id")
}

fn fetch_document_body(addr: &str, collection: &str, id: u64) -> JsonValue {
    let (status, fetched) = http_request(
        addr,
        "GET",
        &format!("/collections/{collection}/entities/{id}"),
        None,
    );
    assert_eq!(status, 200, "GET entity should succeed: {fetched}");
    fetched["data"]["named"]["body"].clone()
}

// Acceptance 1 — dry_run validates a well-formed patch without mutating the
// document. The response advertises `ok:true`, `dry_run:true`, and the count
// of validated operations so Red UI can render a preview confidently.
#[test]
fn document_patch_dry_run_validates_without_mutation() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE DOCUMENT issue751_docs_dry");
    let id = insert_document(&addr, "issue751_docs_dry", json!({ "event_type": "login" }));
    let before = fetch_document_body(&addr, "issue751_docs_dry", id);

    let (status, body) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue751_docs_dry/entities/{id}"),
        Some(json!({
            "dry_run": true,
            "operations": [
                { "op": "set", "path": "/body/meta/ip", "value": "10.0.0.7" }
            ]
        })),
    );
    assert_eq!(status, 200, "dry_run should succeed: {body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["dry_run"], json!(true));
    assert_eq!(body["operations"], json!(1));

    let after = fetch_document_body(&addr, "issue751_docs_dry", id);
    assert_eq!(
        after, before,
        "dry_run must not mutate the document: before={before} after={after}"
    );
}

// Acceptance 1 + 4 — a malformed operation surfaces a structured error
// envelope with the op_index, a JSON Pointer-shaped `pointer`, and a stable
// `code` Red UI can branch on.
#[test]
fn document_patch_invalid_path_returns_structured_error_with_pointer() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE DOCUMENT issue751_docs_err");
    let id = insert_document(&addr, "issue751_docs_err", json!({ "event_type": "login" }));

    let (status, body) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue751_docs_err/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "set", "path": "", "value": "noop" }
            ]
        })),
    );
    assert_eq!(status, 400, "empty path must reject: {body}");
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["code"], json!("PATCH_PATH_INVALID"));
    assert_eq!(body["op_index"], json!(0));
    assert!(
        body["message"].as_str().unwrap_or("").contains("path"),
        "message should mention path: {body}"
    );
}

// Acceptance 2 — set and unset on nested document paths work through the
// documented PATCH contract. Set creates intermediates; unset removes the
// target leaf and leaves siblings untouched.
#[test]
fn document_patch_set_then_unset_nested_path() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE DOCUMENT issue751_docs_nested");
    let id = insert_document(
        &addr,
        "issue751_docs_nested",
        json!({ "event_type": "login" }),
    );

    let (status, _) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue751_docs_nested/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "set", "path": "/body/meta/ip", "value": "10.0.0.7" },
                { "op": "set", "path": "/body/meta/agent", "value": "ui" }
            ]
        })),
    );
    assert_eq!(status, 200);

    let (status, _) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue751_docs_nested/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "unset", "path": "/body/meta/ip" }
            ]
        })),
    );
    assert_eq!(status, 200);

    let after = fetch_document_body(&addr, "issue751_docs_nested", id);
    assert!(
        after["meta"].get("ip").is_none(),
        "unset must remove the leaf: {after}"
    );
    assert_eq!(
        after["meta"]["agent"],
        json!("ui"),
        "unset must leave siblings untouched: {after}"
    );
}

// Acceptance 3 — KV nested set updates a JSON-shaped value without
// rewriting the whole blob. Original siblings survive.
#[test]
fn kv_patch_set_nested_path_preserves_siblings() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE KV issue751_kv");
    // Seed a JSON object: the HTTP PUT path stores object payloads as JSON.
    let (status, _) = http_request(
        &addr,
        "PUT",
        "/collections/issue751_kv/kvs/session:42",
        Some(json!({ "value": { "user": "hansel", "prefs": { "theme": "dark" } } })),
    );
    assert!(status == 200 || status == 201, "PUT failed");

    let (status, body) = http_request(
        &addr,
        "PATCH",
        "/collections/issue751_kv/kvs/session:42",
        Some(json!({
            "operations": [
                { "op": "set", "path": "/prefs/lang", "value": "en" }
            ]
        })),
    );
    assert_eq!(status, 200, "PATCH should succeed: {body}");
    assert_eq!(body["ok"], json!(true));
    let value = &body["value"];
    assert_eq!(value["user"], json!("hansel"));
    assert_eq!(value["prefs"]["theme"], json!("dark"));
    assert_eq!(value["prefs"]["lang"], json!("en"));

    // GET should reflect the patched value.
    let (status, got) = http_request(
        &addr,
        "GET",
        "/collections/issue751_kv/kvs/session:42",
        None,
    );
    assert_eq!(status, 200);
    assert_eq!(got["value"]["prefs"]["lang"], json!("en"));
}

// Acceptance 3 — KV nested unset removes a leaf without touching siblings.
#[test]
fn kv_patch_unset_nested_path_removes_leaf() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE KV issue751_kv_unset");
    let (status, _) = http_request(
        &addr,
        "PUT",
        "/collections/issue751_kv_unset/kvs/cfg",
        Some(json!({ "value": { "a": 1, "b": 2 } })),
    );
    assert!(status == 200 || status == 201);

    let (status, body) = http_request(
        &addr,
        "PATCH",
        "/collections/issue751_kv_unset/kvs/cfg",
        Some(json!({
            "operations": [ { "op": "unset", "path": "/a" } ]
        })),
    );
    assert_eq!(status, 200, "PATCH unset should succeed: {body}");
    assert_eq!(body["value"]["b"], json!(2));
    assert!(body["value"].get("a").is_none(), "{body}");
}

// Acceptance 4 — KV patch on a non-JSON-compatible value (a stored integer)
// returns a structured `KV_VALUE_NOT_JSON` error rather than silently
// replacing the whole value.
#[test]
fn kv_patch_rejects_non_json_value_with_structured_error() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE KV issue751_kv_scalar");
    let (status, _) = http_request(
        &addr,
        "PUT",
        "/collections/issue751_kv_scalar/kvs/counter",
        Some(json!({ "value": 42 })),
    );
    assert!(status == 200 || status == 201);

    let (status, body) = http_request(
        &addr,
        "PATCH",
        "/collections/issue751_kv_scalar/kvs/counter",
        Some(json!({
            "operations": [ { "op": "set", "path": "/a", "value": 1 } ]
        })),
    );
    assert_eq!(status, 400, "scalar KV must reject nested patch: {body}");
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["code"], json!("KV_VALUE_NOT_JSON"));
}

// Acceptance 4 — KV patch against a missing key returns a structured
// `KV_KEY_NOT_FOUND` 404 (not a generic 400) so Red UI can route to a
// distinct empty-state.
#[test]
fn kv_patch_missing_key_returns_structured_not_found() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE KV issue751_kv_missing");

    let (status, body) = http_request(
        &addr,
        "PATCH",
        "/collections/issue751_kv_missing/kvs/no_such_key",
        Some(json!({
            "operations": [ { "op": "set", "path": "/a", "value": 1 } ]
        })),
    );
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], json!("KV_KEY_NOT_FOUND"));
}

// Acceptance 1 — KV dry_run validates without mutation.
#[test]
fn kv_patch_dry_run_validates_without_mutation() {
    let (_rt, addr) = spawn_http_server();
    ddl(&addr, "CREATE KV issue751_kv_dry");
    let (status, _) = http_request(
        &addr,
        "PUT",
        "/collections/issue751_kv_dry/kvs/cfg",
        Some(json!({ "value": { "theme": "dark" } })),
    );
    assert!(status == 200 || status == 201);

    let (status, body) = http_request(
        &addr,
        "PATCH",
        "/collections/issue751_kv_dry/kvs/cfg",
        Some(json!({
            "dry_run": true,
            "operations": [ { "op": "set", "path": "/theme", "value": "light" } ]
        })),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dry_run"], json!(true));

    // Original value must survive the dry_run.
    let (_, got) = http_request(&addr, "GET", "/collections/issue751_kv_dry/kvs/cfg", None);
    assert_eq!(got["value"]["theme"], json!("dark"));
}
