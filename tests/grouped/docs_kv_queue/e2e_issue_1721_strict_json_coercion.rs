// Regression coverage for issue #1721 — ADR 0067 point 9: no runtime
// string→JSON coercion. The parser already rejects a quoted-string document
// body literal; these tests pin the two *runtime* positions the parser cannot
// see — a parameter-bound document body and a bare string bound to a
// JSON-typed table column — and prove `JSON_PARSE(<expr>)` is the sanctioned
// escape hatch for both.

#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

// --- Document body via parameter -------------------------------------------

#[test]
fn document_body_param_bound_string_is_rejected() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1721_events")
        .expect("create document");

    let err = rt
        .execute_query_with_params(
            "INSERT INTO issue1721_events DOCUMENT VALUES ($1)",
            &[Value::Text(r#"{"level":"info"}"#.into())],
        )
        .expect_err("a parameter-bound string body must be rejected, not coerced");
    let message = err.to_string();
    assert!(
        message.contains("JSON_PARSE"),
        "error should point at JSON_PARSE, got: {message}"
    );
}

#[test]
fn document_body_via_json_parse_is_accepted() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1721_events_ok")
        .expect("create document");

    // The sanctioned escape hatch: wrap the runtime string in JSON_PARSE.
    rt.execute_query(
        "INSERT INTO issue1721_events_ok DOCUMENT VALUES (JSON_PARSE('{\"level\":\"warn\"}'))",
    )
    .expect("JSON_PARSE-wrapped body must be accepted");
}

// --- JSON-typed table column ------------------------------------------------

#[test]
fn json_column_string_literal_is_rejected() {
    let rt = runtime();
    rt.execute_query("CREATE TABLE issue1721_things (id INTEGER, payload JSON)")
        .expect("create table");

    let err = rt
        .execute_query("INSERT INTO issue1721_things (id, payload) VALUES (1, '{\"a\":1}')")
        .expect_err("a bare string bound to a JSON column must be rejected, not coerced");
    let message = err.to_string();
    assert!(
        message.contains("JSON_PARSE"),
        "error should point at JSON_PARSE, got: {message}"
    );
}

#[test]
fn json_column_via_json_parse_is_accepted() {
    let rt = runtime();
    rt.execute_query("CREATE TABLE issue1721_things_ok (id INTEGER, payload JSON)")
        .expect("create table");

    rt.execute_query(
        "INSERT INTO issue1721_things_ok (id, payload) VALUES (1, JSON_PARSE('{\"a\":1}'))",
    )
    .expect("JSON_PARSE-wrapped JSON column value must be accepted");
}
