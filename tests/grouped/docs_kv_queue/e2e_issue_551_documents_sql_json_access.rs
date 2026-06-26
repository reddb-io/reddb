// Regression coverage for issue #551 — Documents: SQL JSON access.
//
// Each test maps to one bullet in the issue's `## Acceptance` list:
//   - `SELECT body.field FROM doc_collection WHERE body.field = ?` works.
//   - `json_extract(body, '$.path')` parses and evaluates.
//   - `CONTAINS` operator works for arrays in WHERE.
//   - Combined SELECT/WHERE case using `body.field` access.
//
// User stories: PRD #449 #11, #12, #13. The SQL surface is the
// dotted field-access form (`body.level`), the explicit
// `json_extract(body, '$.level')` form, and the array-membership
// `CONTAINS(body.tags, 'checkout')` form on a document collection.

use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn text_field<'a>(row: &'a UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text {field}, got {other:?} in {row:?}"),
    }
}

// Seed three documents with a known shape:
//   { "level": "info" | "warn", "tags": [...], "seq": <int> }
// Two of them carry the string "checkout" inside `tags` so the
// `CONTAINS` acceptance test can pick them out by array membership.
fn seed_documents(rt: &RedDBRuntime, collection: &str) {
    rt.execute_query(&format!("CREATE DOCUMENT {collection}"))
        .expect("CREATE DOCUMENT");
    let docs = [
        r#"{"level":"info","tags":["page_view","checkout"],"seq":1}"#,
        r#"{"level":"warn","tags":["page_view"],"seq":2}"#,
        r#"{"level":"info","tags":["checkout","logout"],"seq":3}"#,
    ];
    for d in docs {
        rt.execute_query(&format!(
            "INSERT INTO {collection} DOCUMENT (body) VALUES ('{d}')"
        ))
        .expect("INSERT DOCUMENT");
    }
}

// Bullet 1: `SELECT body.field FROM doc_collection WHERE body.field = ?`.
// The projection materialises the JSON leaf and the WHERE clause filters
// down to matching rows.
#[test]
fn sql_select_body_field_with_where_returns_matching_rows() {
    let rt = runtime();
    seed_documents(&rt, "issue551_body_field");

    let page = rt
        .execute_query(
            "SELECT body.level FROM issue551_body_field \
             WHERE body.level = 'info'",
        )
        .expect("SELECT body.field WHERE body.field = ? should succeed");
    assert_eq!(
        page.result.records.len(),
        2,
        "WHERE body.level = 'info' should match two rows; got {:?}",
        page.result.records
    );
    for record in &page.result.records {
        assert_eq!(text_field(record, "body.LEVEL"), "info");
    }
}

// Bullet 2: `json_extract(body, '$.path')` parses and evaluates.
// This is the explicit alternative to dot-notation — it must work for
// both SELECT projection and WHERE filtering.
#[test]
fn sql_json_extract_in_select_projects_leaf_value() {
    let rt = runtime();
    seed_documents(&rt, "issue551_json_extract_select");

    let page = rt
        .execute_query(
            "SELECT json_extract(body, '$.level') FROM issue551_json_extract_select \
             WHERE level = 'warn'",
        )
        .expect("json_extract projection should succeed");
    assert_eq!(
        page.result.records.len(),
        1,
        "WHERE level = 'warn' should match one row; got {:?}",
        page.result.records
    );
    let record = &page.result.records[0];
    // An unaliased function projection is labeled with its source-text form
    // (#1370): `json_extract(body, '$.level')` lowers to the `JSON_EXTRACT`
    // builtin, so the output column reads `JSON_EXTRACT(body, '$.level')`.
    // The contract this test pins is that the function dispatched and the
    // path resolved — not the exact column header — so we look the value up
    // under the reconstructed label.
    let label = "JSON_EXTRACT(body, '$.level')";
    match record.get(label) {
        Some(Value::Text(value)) => {
            // JSON_EXTRACT returns the raw JSON encoding of the leaf,
            // so a string value comes back quoted. Both wrapped and
            // unwrapped forms count as "evaluated correctly" — what
            // the contract pins is that the function name dispatched
            // and the path resolved.
            let raw = value.as_ref();
            assert!(
                raw == "\"warn\"" || raw == "warn",
                "expected warn (raw or quoted), got {raw:?}"
            );
        }
        other => panic!("expected text json_extract result, got {other:?} in {record:?}"),
    }
}

#[test]
fn sql_json_extract_in_where_filters_rows() {
    let rt = runtime();
    seed_documents(&rt, "issue551_json_extract_where");

    let page = rt
        .execute_query(
            "SELECT seq FROM issue551_json_extract_where \
             WHERE json_extract(body, '$.level') = '\"info\"'",
        )
        .expect("json_extract in WHERE should succeed");
    assert_eq!(
        page.result.records.len(),
        2,
        "json_extract WHERE should match the two info rows; got {:?}",
        page.result.records
    );
}

// Bullet 3: `body.tags CONTAINS 'checkout'` operator form. PRD user
// story #13 calls this surface out explicitly — array membership as
// an infix WHERE predicate on a document body's tag array. The
// probabilistic SQL-read form (`CONTAINS(element)` against a
// `FILTER` collection, slice #554) is a sibling surface, not the
// document path.
#[test]
fn sql_contains_operator_filters_document_arrays() {
    let rt = runtime();
    seed_documents(&rt, "issue551_contains_op");

    let page = rt
        .execute_query(
            "SELECT seq FROM issue551_contains_op \
             WHERE body.tags CONTAINS 'checkout'",
        )
        .expect("body.tags CONTAINS 'checkout' should succeed");
    assert_eq!(
        page.result.records.len(),
        2,
        "infix CONTAINS should match the two rows whose tags include checkout; got {:?}",
        page.result.records
    );
}

// Bullet 4: combined SELECT/WHERE case. Project `body.level` and
// filter on `body.tags CONTAINS 'checkout'` in the same statement so
// the dotted projection and the array-membership filter cooperate.
#[test]
fn sql_combined_select_body_field_with_contains_where() {
    let rt = runtime();
    seed_documents(&rt, "issue551_combined");

    let page = rt
        .execute_query(
            "SELECT body.level, seq FROM issue551_combined \
             WHERE body.tags CONTAINS 'checkout' ORDER BY seq",
        )
        .expect("combined dotted projection + CONTAINS filter should succeed");
    assert_eq!(
        page.result.records.len(),
        2,
        "combined query should match the two checkout rows; got {:?}",
        page.result.records
    );
    let seqs: Vec<i64> = page
        .result
        .records
        .iter()
        .map(|record| match record.get("seq") {
            Some(Value::Integer(n)) => *n,
            Some(Value::Float(n)) => *n as i64,
            Some(Value::UnsignedInteger(n)) => *n as i64,
            other => panic!("expected numeric seq, got {other:?}"),
        })
        .collect();
    assert_eq!(seqs, vec![1, 3]);
    for record in &page.result.records {
        assert_eq!(text_field(record, "body.LEVEL"), "info");
    }
}
