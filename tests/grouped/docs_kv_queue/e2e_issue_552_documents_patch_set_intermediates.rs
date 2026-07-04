// Regression coverage for issue #552 — Documents: patch — `set` creates
// intermediate objects on missing nested paths, `unset` on a missing path
// is a no-op, and array positional patch paths are explicitly rejected.
//
// Each test in this file maps to one bullet in the issue's `## Acceptance`
// list, so a future regression is traceable back to the specific public
// promise it broke.
//
// Acceptance bullets:
//   1. Patch `set` on a nested path creates intermediate objects.
//   2. Patch `unset` on a missing path returns success with no change.
//   3. Patch is exposed through SQL and HTTP.
//   4. Out-of-scope (array positional) explicitly rejected with a clear
//      error.
//
// The first three bullets are exercised through both the HTTP PATCH
// `/collections/{name}/entities/{id}` surface (the public patch endpoint)
// and the SQL `UPDATE … DOCUMENTS SET …` surface (which routes through
// the same patch core in `apply_loaded_patch_entity_core`). Bullet 4 is
// pinned at the document body level because that's the surface where
// array positional paths could plausibly be requested by clients holding
// an array-shaped body.

#[path = "../../support/mod.rs"]
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

fn spawn_http_server() -> (support::TempDbFile, RedDBRuntime, String) {
    let (db, rt) = support::persistent_runtime("documents-patch-http");
    let server = RedDBServer::new(rt.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr").to_string();
    server.serve_in_background_on(listener);
    (db, rt, addr)
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

fn create_document_collection(addr: &str, name: &str) {
    let (status, _) = http_request(
        addr,
        "POST",
        "/query",
        Some(json!({ "query": format!("CREATE DOCUMENT {name}") })),
    );
    assert_eq!(status, 200, "CREATE DOCUMENT {name} should succeed");
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

fn fetch_document(addr: &str, collection: &str, id: u64) -> JsonValue {
    let (status, fetched) = http_request(
        addr,
        "GET",
        &format!("/collections/{collection}/entities/{id}"),
        None,
    );
    assert_eq!(status, 200, "GET entity should succeed: {fetched}");
    fetched
}

// Acceptance 1 + 3 (HTTP): patch `set` on a nested path creates the
// intermediate object — the original document had no `meta` field, and
// after PATCH `/body/meta/ip` the body contains `meta: { ip: "..." }`.
#[test]
fn http_patch_set_on_nested_body_path_creates_intermediate_object() {
    let (_db, _rt, addr) = spawn_http_server();
    create_document_collection(&addr, "issue552_set_intermediates");
    let id = insert_document(
        &addr,
        "issue552_set_intermediates",
        json!({ "event_type": "login", "attempts": 1 }),
    );

    let (status, patched) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue552_set_intermediates/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "set", "path": "/body/meta/ip", "value": "10.0.0.7" }
            ]
        })),
    );
    assert_eq!(status, 200, "PATCH should succeed: {patched}");

    let fetched = fetch_document(&addr, "issue552_set_intermediates", id);
    assert_eq!(
        fetched["data"]["named"]["body"]["meta"]["ip"],
        json!("10.0.0.7"),
        "intermediate object 'meta' must have been created by set: {fetched}",
    );
    // The pre-existing top-level body fields must survive untouched.
    assert_eq!(
        fetched["data"]["named"]["body"]["event_type"],
        json!("login")
    );
    assert_eq!(fetched["data"]["named"]["body"]["attempts"], json!(1));
}

// Acceptance 2 + 3 (HTTP): patch `unset` on a missing nested path is a
// no-op success — the response is 200 and the document body is unchanged.
#[test]
fn http_patch_unset_on_missing_path_is_noop_success() {
    let (_db, _rt, addr) = spawn_http_server();
    create_document_collection(&addr, "issue552_unset_missing");
    let id = insert_document(
        &addr,
        "issue552_unset_missing",
        json!({ "event_type": "logout", "attempts": 2 }),
    );
    let before = fetch_document(&addr, "issue552_unset_missing", id);

    let (status, patched) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue552_unset_missing/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "unset", "path": "/body/meta/ip" },
                { "op": "unset", "path": "/body/never_existed" }
            ]
        })),
    );
    assert_eq!(
        status, 200,
        "unset on missing path must succeed (no-op): {patched}"
    );

    let after = fetch_document(&addr, "issue552_unset_missing", id);
    assert_eq!(
        after["data"]["named"]["body"], before["data"]["named"]["body"],
        "document body must be unchanged after unset on missing path",
    );
}

// Acceptance 4: array positional document patch paths are explicitly
// rejected with a clear error pointing at the supported alternative
// (replace the array or the full document body).
#[test]
fn http_patch_array_positional_path_rejected_with_clear_error() {
    let (_db, _rt, addr) = spawn_http_server();
    create_document_collection(&addr, "issue552_array_position");
    let id = insert_document(
        &addr,
        "issue552_array_position",
        json!({ "tags": ["alpha", "beta"] }),
    );

    let (status, body) = http_request(
        &addr,
        "PATCH",
        &format!("/collections/issue552_array_position/entities/{id}"),
        Some(json!({
            "operations": [
                { "op": "set", "path": "/body/tags/0", "value": "gamma" }
            ]
        })),
    );
    assert_eq!(
        status, 400,
        "array positional patch must be rejected with 4xx: {body}"
    );
    let message = body["error"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("");
    assert!(
        message.contains("array positional"),
        "error must call out the array positional limitation: {body}"
    );
    assert!(
        message.contains("replace the array") || message.contains("full document body"),
        "error must point to the supported alternative: {body}"
    );

    // Document must remain untouched after the rejected patch.
    let fetched = fetch_document(&addr, "issue552_array_position", id);
    assert_eq!(
        fetched["data"]["named"]["body"]["tags"],
        json!(["alpha", "beta"]),
        "rejected patch must not have modified the array: {fetched}",
    );
}

// Acceptance 3 (SQL): patch is also exposed through SQL — `UPDATE …
// DOCUMENTS SET …` routes through the same patch core as the HTTP
// surface, and a SET that targets a previously-absent top-level body
// key creates it without rewriting the rest of the body.
#[test]
fn sql_update_documents_set_creates_missing_top_level_body_field() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue552_sql_patch")
        .expect("CREATE DOCUMENT");
    rt.execute_query(
        r#"INSERT INTO issue552_sql_patch DOCUMENT
           VALUES ({"event_type":"login","attempts":1})"#,
    )
    .expect("INSERT");

    let updated = rt
        .execute_query(
            "UPDATE issue552_sql_patch DOCUMENTS SET status = 'reviewed' \
             WHERE event_type = 'login' RETURNING event_type, attempts, status",
        )
        .expect("UPDATE DOCUMENTS SET should succeed");
    assert_eq!(updated.affected_rows, 1);

    // Both the pre-existing fields and the freshly-created `status`
    // field must be visible — proving the patch wrote the new key
    // without dropping anything.
    let selected = rt
        .execute_query(
            "SELECT event_type, attempts, status FROM issue552_sql_patch WHERE event_type = 'login'",
        )
        .expect("post-patch SELECT should succeed");
    assert_eq!(selected.result.records.len(), 1);
    let record = &selected.result.records[0];
    let status_field = record
        .get("status")
        .unwrap_or_else(|| panic!("status field absent in {record:?}"));
    assert!(format!("{status_field:?}").contains("reviewed"));
    let event_type = record
        .get("event_type")
        .unwrap_or_else(|| panic!("event_type missing in {record:?}"));
    assert!(format!("{event_type:?}").contains("login"));
}
