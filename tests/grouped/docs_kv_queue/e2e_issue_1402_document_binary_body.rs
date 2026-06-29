// Issue #1402 — DOCUMENT: binary body write + binary→JSON read decode
// (PRD-1398 / ADR-0063).
//
// Acceptance bullets:
//   1. By default, a written document is stored as a binary body
//      container (the stored `body` bytes start with the `RDOC` magic).
//   2. `SELECT body` and field projection return JSON equal to today's
//      behaviour.
//   3. Rich types (Email, Ipv4, Subnet, Color, …) survive write→read.
//   4. Existing document RQL behaviour (json_extract, body.field, UPDATE SET,
//      PATCH, reopen) is unchanged with the flag on.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use support::PersistentDbPath;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn enable_binary_body(rt: &RedDBRuntime) {
    rt.execute_query("SET CONFIG storage.binary_document_body = true")
        .expect("enable storage.binary_document_body");
}

fn text_field<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> String {
    match row.get(field) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

/// Extract a top-level body field via `json_extract`, returning its compact
/// JSON text (strings come back quoted). This exercises the binary→JSON decode
/// path and avoids the case-folding of dotted `body.field` column names.
fn body_json_extract(rt: &RedDBRuntime, collection: &str, field: &str) -> String {
    let out = rt
        .execute_query(&format!(
            "SELECT json_extract(body, '$.{field}') FROM {collection}"
        ))
        .expect("json_extract");
    let column = format!("JSON_EXTRACT(body, '$.{field}')");
    match out.result.records[0].get(&column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text at {column}, got {other:?}"),
    }
}

/// The single stored record `body` field as raw storage bytes.
fn body_bytes(rt: &RedDBRuntime, collection: &str) -> Vec<u8> {
    let page = rt
        .scan_collection(collection, None, 1)
        .expect("scan_collection");
    assert_eq!(page.items.len(), 1, "exactly one document");
    let row = page.items[0].data.as_row().expect("row");
    match row.get_field("body") {
        Some(Value::Json(bytes)) => bytes.clone(),
        other => panic!("expected body Json, got {other:?}"),
    }
}

const RDOC_MAGIC: &[u8; 4] = b"RDOC";

#[test]
fn flag_on_stores_body_as_binary_container() {
    let rt = runtime();
    enable_binary_body(&rt);
    rt.execute_query("CREATE DOCUMENT events").expect("create");
    rt.execute_query(
        r#"INSERT INTO events DOCUMENT (body) VALUES ('{"event_type":"login","attempts":2}')"#,
    )
    .expect("insert");

    let bytes = body_bytes(&rt, "events");
    assert_eq!(
        &bytes[..4],
        RDOC_MAGIC,
        "flag on must store the native binary container"
    );
}

#[test]
fn default_stores_body_as_binary_container() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT events").expect("create");
    rt.execute_query(
        r#"INSERT INTO events DOCUMENT (body) VALUES ('{"event_type":"login","attempts":2}')"#,
    )
    .expect("insert");

    let bytes = body_bytes(&rt, "events");
    assert_eq!(
        &bytes[..4],
        RDOC_MAGIC,
        "default must store the native binary container"
    );
}

/// Run the same document script with the default and with the flag explicitly
/// set to true, and assert every observable RQL result matches — `SELECT
/// body.field`, `json_extract`, bare-field projection, and a tag-array CONTAINS
/// predicate.
#[test]
fn explicit_true_observable_behaviour_matches_default() {
    fn observe(explicit_true: bool) -> Vec<(String, String, String, usize)> {
        let rt = runtime();
        if explicit_true {
            enable_binary_body(&rt);
        }
        rt.execute_query("CREATE DOCUMENT docs").expect("create");
        for doc in [
            r#"{"level":"info","seq":1,"tags":["page_view","checkout"]}"#,
            r#"{"level":"warn","seq":2,"tags":["page_view"]}"#,
            r#"{"level":"info","seq":3,"tags":["checkout","logout"]}"#,
        ] {
            rt.execute_query(&format!(
                "INSERT INTO docs DOCUMENT (body) VALUES ('{doc}')"
            ))
            .expect("insert");
        }

        // body.field access + json_extract + bare-field projection.
        let rows = rt
            .execute_query(
                "SELECT level, body.level, json_extract(body, '$.level') \
                 FROM docs WHERE body.level = 'info' ORDER BY seq",
            )
            .expect("select");
        let mut observed = Vec::new();
        for row in &rows.result.records {
            observed.push((
                text_field(row, "level"),
                text_field(row, "body.LEVEL"),
                match row.get("JSON_EXTRACT(body, '$.level')") {
                    Some(Value::Text(v)) => v.to_string(),
                    other => format!("{other:?}"),
                },
                rows.result.records.len(),
            ));
        }

        // Array-membership predicate over the document body's tag array.
        let contains = rt
            .execute_query("SELECT seq FROM docs WHERE body.tags CONTAINS 'checkout' ORDER BY seq")
            .expect("contains");
        observed.push((
            "contains_count".to_string(),
            contains.result.records.len().to_string(),
            String::new(),
            0,
        ));
        observed
    }

    assert_eq!(
        observe(false),
        observe(true),
        "explicit true reads must match default reads"
    );
}

/// Rich semantic types arrive on the JSON wire as strings; the binary body
/// must round-trip exactly the JSON the client sent.
#[test]
fn rich_types_survive_write_then_read() {
    let rt = runtime();
    enable_binary_body(&rt);
    rt.execute_query("CREATE DOCUMENT assets").expect("create");
    rt.execute_query(
        r##"INSERT INTO assets DOCUMENT (body) VALUES ('{"email":"user@example.com","ipv4":"127.0.0.1","subnet":"10.0.0.0/8","color":"#DEADBE"}')"##,
    )
    .expect("insert");

    let bytes = body_bytes(&rt, "assets");
    assert_eq!(&bytes[..4], RDOC_MAGIC, "stored binary");

    // These arrive on the JSON wire as strings; the body must round-trip them
    // exactly (json_extract returns the JSON-quoted form).
    for (field, expected) in [
        ("email", "user@example.com"),
        ("ipv4", "127.0.0.1"),
        ("subnet", "10.0.0.0/8"),
        ("color", "#DEADBE"),
    ] {
        assert_eq!(
            body_json_extract(&rt, "assets", field),
            format!("\"{expected}\""),
            "rich type {field} must survive"
        );
    }
}

/// UPDATE SET on a bare document field keeps `body` in sync when the body is
/// stored as a binary container.
#[test]
fn update_set_keeps_binary_body_in_sync() {
    let rt = runtime();
    enable_binary_body(&rt);
    rt.execute_query("CREATE DOCUMENT users").expect("create");
    rt.execute_query(
        r#"INSERT INTO users DOCUMENT (body) VALUES ('{"name":"Alice","tier":"free"}')"#,
    )
    .expect("insert");

    rt.execute_query("UPDATE users DOCUMENTS SET tier = 'pro' WHERE name = 'Alice'")
        .expect("update");

    // Still binary on disk after the update.
    assert_eq!(&body_bytes(&rt, "users")[..4], RDOC_MAGIC);

    // Bare-field projection and body agree under binary storage.
    let col = rt
        .execute_query("SELECT tier FROM users")
        .expect("select tier");
    assert_eq!(text_field(&col.result.records[0], "tier"), "pro");
    assert_eq!(body_json_extract(&rt, "users", "tier"), "\"pro\"");
    // The untargeted field must survive.
    assert_eq!(body_json_extract(&rt, "users", "name"), "\"Alice\"");
}

/// A binary body persists across a checkpoint + reopen and still reads as JSON.
#[test]
fn binary_body_survives_reopen() {
    let path = PersistentDbPath::new("issue1402_binary_body");
    {
        let rt = path.open_runtime();
        enable_binary_body(&rt);
        rt.execute_query("CREATE DOCUMENT events").expect("create");
        rt.execute_query(
            r#"INSERT INTO events DOCUMENT (body) VALUES ('{"label":"hello","n":7}')"#,
        )
        .expect("insert");
        assert_eq!(&body_bytes(&rt, "events")[..4], RDOC_MAGIC);
    }
    let rt = path.open_runtime();
    assert_eq!(body_json_extract(&rt, "events", "label"), "\"hello\"");
}
