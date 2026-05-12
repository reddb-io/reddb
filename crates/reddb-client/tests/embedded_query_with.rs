//! Integration coverage for `EmbeddedClient::query_with` (#364).
//!
//! Exercises int/text/null/vector params end-to-end through the
//! engine's `user_params::bind` path. Mirrors the cross-driver
//! integration contract in PRD #351.

use reddb_client::embedded::EmbeddedClient;
use reddb_client::{JsonValue, Value};

fn open() -> EmbeddedClient {
    EmbeddedClient::in_memory().expect("memory db")
}

fn make_users(c: &EmbeddedClient) {
    c.query("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER, score FLOAT)")
        .expect("create users");
    c.insert(
        "users",
        &JsonValue::object([
            ("id", JsonValue::number(1)),
            ("name", JsonValue::string("Alice")),
            ("age", JsonValue::number(30)),
            ("score", JsonValue::number(9.5)),
        ]),
    )
    .expect("insert alice");
    c.insert(
        "users",
        &JsonValue::object([
            ("id", JsonValue::number(2)),
            ("name", JsonValue::string("Bob")),
            ("age", JsonValue::number(25)),
            ("score", JsonValue::number(7.1)),
        ]),
    )
    .expect("insert bob");
}

#[tokio::test]
async fn query_with_int_param_matches_one_row() {
    let c = open();
    make_users(&c);
    let r = c
        .query_with("SELECT * FROM users WHERE id = $1", &[Value::Int(2)])
        .expect("query_with int");
    assert_eq!(r.rows.len(), 1);
    let name = r.rows[0]
        .iter()
        .find(|(k, _)| k == "name")
        .map(|(_, v)| format!("{v}"))
        .unwrap();
    assert_eq!(name, "Bob");
}

#[tokio::test]
async fn query_with_text_and_null_params() {
    let c = open();
    make_users(&c);
    let r = c
        .query_with(
            "SELECT * FROM users WHERE name = $1 AND $2 IS NULL",
            &[Value::Text("Alice".into()), Value::Null],
        )
        .expect("query_with text+null");
    assert_eq!(r.rows.len(), 1);
}

#[tokio::test]
async fn query_with_limit_param_bounds_results() {
    let c = open();
    make_users(&c);
    let r = c
        .query_with("SELECT * FROM users LIMIT $1", &[Value::Int(1)])
        .expect("query_with limit");
    assert_eq!(r.rows.len(), 1);
}

#[tokio::test]
async fn query_with_vector_param_serializes_to_engine_value() {
    // Tracer half of #355 — `Value::Vector` survives the driver-side
    // conversion to the engine's `SchemaValue::Vector`. Full executor
    // round-trip for `SEARCH SIMILAR $1` is covered by the engine-side
    // `user_params::bind_search_similar_vector_param` test; the driver
    // contract verified here is that the conversion preserves bytes.
    let v = Value::Vector(vec![0.1, 0.2, 0.3]);
    let schema = v.into_schema_value();
    match schema {
        reddb_server::storage::schema::Value::Vector(v) => {
            assert_eq!(v, vec![0.1f32, 0.2, 0.3]);
        }
        other => panic!("expected SV::Vector, got {other:?}"),
    }
}

#[tokio::test]
async fn query_with_empty_params_routes_to_legacy_path() {
    let c = open();
    make_users(&c);
    let r = c
        .query_with("SELECT * FROM users", &[] as &[Value])
        .expect("query_with empty");
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn query_with_arity_mismatch_surfaces_error() {
    let c = open();
    make_users(&c);
    let err = c
        .query_with(
            "SELECT * FROM users WHERE id = $1 AND name = $2",
            &[Value::Int(1)],
        )
        .expect_err("arity mismatch should error");
    let msg = err.to_string();
    assert!(
        msg.contains("parameters") || msg.contains("expects"),
        "got {msg}"
    );
}

#[tokio::test]
async fn query_with_intovalue_ergonomic_form() {
    // Reddb::query_with takes anything implementing IntoValue. We don't
    // construct a Reddb here (avoids the async URL parser) — instead we
    // pre-build Values via IntoValue and feed them to EmbeddedClient
    // directly, which is the same path Reddb::Embedded forwards through.
    use reddb_client::IntoValue;
    let c = open();
    make_users(&c);
    let params: Vec<Value> = vec![1i64.into_value(), "Alice".into_value()];
    let r = c
        .query_with("SELECT * FROM users WHERE id = $1 AND name = $2", &params)
        .expect("query_with intovalue");
    assert_eq!(r.rows.len(), 1);
}
