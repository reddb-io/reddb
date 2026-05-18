// Regression coverage for issue #550 — Documents: list/filter with pagination.
//
// Each test maps to one bullet in the issue's `## Acceptance` list:
//   - SQL list with WHERE + LIMIT/OFFSET returns the expected slice.
//   - HTTP list endpoint supports the same filter + pagination.
//   - Empty pages, beyond-end pages, and filtered pages all behave.
//
// User stories: PRD #449 #8 and #17. The SQL surface is
// `SELECT ... FROM <doc_collection> WHERE ... LIMIT N OFFSET M` and the
// HTTP surface is `GET /collections/<name>/scan?offset=&limit=` for cursor
// pagination, with the same `/query` SQL endpoint for the filtered form.

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};

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
        Some(Value::UnsignedInteger(value)) => *value as f64,
        other => panic!("expected number {field}, got {other:?} in {row:?}"),
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

// Seed N documents into `collection`, half with kind="login", half with
// kind="logout". `i` is encoded into the body so tests can assert which
// rows landed in a given page.
fn seed_documents(rt: &RedDBRuntime, collection: &str, count: usize) {
    rt.execute_query(&format!("CREATE DOCUMENT {collection}"))
        .expect("CREATE DOCUMENT");
    for i in 0..count {
        let kind = if i % 2 == 0 { "login" } else { "logout" };
        let payload = format!(
            r#"{{"event_type":"{kind}","seq":{i},"meta":{{"i":{i}}}}}"#
        );
        rt.execute_query(&format!(
            "INSERT INTO {collection} DOCUMENT (body) VALUES ('{payload}')"
        ))
        .expect("INSERT DOCUMENT");
    }
}

// Acceptance: SQL list with WHERE + LIMIT/OFFSET returns the expected slice.
#[test]
fn sql_select_with_where_limit_offset_returns_expected_slice() {
    let rt = runtime();
    seed_documents(&rt, "issue550_sql_basic", 10);

    // 10 docs total. seq=0,2,4,6,8 are "login" (5 docs). With LIMIT 2
    // OFFSET 1 ordered by seq we expect seq=2 and seq=4.
    let page = rt
        .execute_query(
            "SELECT event_type, seq FROM issue550_sql_basic \
             WHERE event_type = 'login' ORDER BY seq LIMIT 2 OFFSET 1",
        )
        .expect("filtered LIMIT/OFFSET page should succeed");
    assert_eq!(
        page.result.records.len(),
        2,
        "LIMIT 2 should bound the page to 2 rows; got {:?}",
        page.result.records
    );
    assert_eq!(text_field(&page.result.records[0], "event_type"), "login");
    assert_eq!(number_field(&page.result.records[0], "seq"), 2.0);
    assert_eq!(number_field(&page.result.records[1], "seq"), 4.0);
}

// Acceptance: empty filter (WHERE matches nothing) returns an empty page.
#[test]
fn sql_select_with_filter_that_matches_nothing_returns_empty_page() {
    let rt = runtime();
    seed_documents(&rt, "issue550_sql_empty_filter", 4);

    let page = rt
        .execute_query(
            "SELECT event_type, seq FROM issue550_sql_empty_filter \
             WHERE event_type = 'never_inserted' LIMIT 5 OFFSET 0",
        )
        .expect("non-matching WHERE should succeed");
    assert!(
        page.result.records.is_empty(),
        "non-matching filter should yield zero rows; got {:?}",
        page.result.records
    );
}

// Acceptance: beyond-end OFFSET returns an empty page (not an error).
#[test]
fn sql_select_beyond_end_offset_returns_empty_page() {
    let rt = runtime();
    seed_documents(&rt, "issue550_sql_beyond_end", 3);

    let page = rt
        .execute_query(
            "SELECT event_type, seq FROM issue550_sql_beyond_end \
             ORDER BY seq LIMIT 10 OFFSET 100",
        )
        .expect("OFFSET past the end should succeed");
    assert!(
        page.result.records.is_empty(),
        "OFFSET past the end should return zero rows; got {:?}",
        page.result.records
    );
}

// Acceptance: HTTP list endpoint supports pagination (offset + limit).
#[test]
fn http_scan_offset_and_limit_paginates_documents() {
    let (rt, addr) = spawn_http_server();
    seed_documents(&rt, "issue550_http_scan", 5);

    // First page: limit=2, offset=0.
    let (status, first) = http_request(
        &addr,
        "GET",
        "/collections/issue550_http_scan/scan?offset=0&limit=2",
        None,
    );
    assert_eq!(status, 200, "first={first}");
    let items = first["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "first page should hold 2 items; got {first}");

    // Second page: limit=2, offset=2. Should give 2 more items, with a
    // disjoint id set vs the first page.
    let (status, second) = http_request(
        &addr,
        "GET",
        "/collections/issue550_http_scan/scan?offset=2&limit=2",
        None,
    );
    assert_eq!(status, 200, "second={second}");
    let second_items = second["items"].as_array().expect("items array");
    assert_eq!(
        second_items.len(),
        2,
        "second page should hold 2 items; got {second}"
    );

    let collect_ids = |array: &[JsonValue]| -> Vec<u64> {
        array
            .iter()
            .filter_map(|entry| entry["id"].as_u64())
            .collect()
    };
    let first_ids = collect_ids(items);
    let second_ids = collect_ids(second_items);
    for id in &first_ids {
        assert!(
            !second_ids.contains(id),
            "second page must not repeat first-page ids: first={first_ids:?}, second={second_ids:?}"
        );
    }
}

// Acceptance: HTTP list endpoint with beyond-end offset returns an empty page.
#[test]
fn http_scan_beyond_end_offset_returns_empty_page() {
    let (rt, addr) = spawn_http_server();
    seed_documents(&rt, "issue550_http_beyond", 3);

    let (status, page) = http_request(
        &addr,
        "GET",
        "/collections/issue550_http_beyond/scan?offset=100&limit=10",
        None,
    );
    assert_eq!(status, 200, "page={page}");
    let items = page["items"].as_array().expect("items array");
    assert!(
        items.is_empty(),
        "offset past the end should yield zero items; got {page}"
    );
}

// Acceptance: HTTP list endpoint supports the same WHERE filter as SQL by
// routing the query through POST /query. This is the documented HTTP form
// of `SELECT ... WHERE ... LIMIT N OFFSET M` on a document collection.
#[test]
fn http_query_with_where_limit_offset_filters_and_paginates() {
    let (rt, addr) = spawn_http_server();
    seed_documents(&rt, "issue550_http_filter", 8);

    // seq=0,2,4,6 are "login" (4 docs). LIMIT 2 OFFSET 1 ORDER BY seq →
    // seq=2 and seq=4.
    let (status, body) = http_request(
        &addr,
        "POST",
        "/query",
        Some(json!({
            "query":
                "SELECT event_type, seq FROM issue550_http_filter \
                 WHERE event_type = 'login' ORDER BY seq LIMIT 2 OFFSET 1"
        })),
    );
    assert_eq!(status, 200, "body={body}");
    let records = body["result"]["records"]
        .as_array()
        .or_else(|| body["records"].as_array())
        .unwrap_or_else(|| panic!("expected records array in {body}"));
    assert_eq!(records.len(), 2, "filtered page should hold 2 rows; got {body}");

    // POST /query result records put projected fields under `values`.
    let field = |record: &JsonValue, name: &str| -> JsonValue {
        record
            .get("values")
            .and_then(|values| values.get(name))
            .cloned()
            .or_else(|| record.get(name).cloned())
            .unwrap_or_else(|| panic!("expected field {name} in {record}"))
    };
    let seq_value = |record: &JsonValue| -> f64 {
        field(record, "seq")
            .as_f64()
            .unwrap_or_else(|| panic!("expected numeric seq in {record}"))
    };
    let event_type = |record: &JsonValue| -> String {
        field(record, "event_type")
            .as_str()
            .map(|value| value.to_string())
            .unwrap_or_else(|| panic!("expected event_type string in {record}"))
    };
    assert_eq!(event_type(&records[0]), "login");
    assert_eq!(seq_value(&records[0]), 2.0);
    assert_eq!(seq_value(&records[1]), 4.0);
}
