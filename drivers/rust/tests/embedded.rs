//! Smoke test for the embedded backend.

use reddb_client::{ErrorCode, JsonValue, Reddb};

#[tokio::test]
async fn connect_memory_then_version() {
    let _db = Reddb::connect("memory://").await.expect("connect");
}

#[tokio::test]
async fn connect_with_invalid_scheme_returns_unsupported() {
    let err = Reddb::connect("mongodb://localhost").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedScheme);
}

#[tokio::test]
async fn connect_with_empty_uri_returns_invalid() {
    let err = Reddb::connect("").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidUri);
}

#[tokio::test]
async fn insert_then_query_round_trip() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let payload = JsonValue::object([
        ("name", JsonValue::string("Alice")),
        ("age", JsonValue::number(30)),
    ]);
    let inserted = db.insert("users", &payload).await.expect("insert");
    assert_eq!(inserted.affected, 1);

    let payload2 = JsonValue::object([
        ("name", JsonValue::string("Bob")),
        ("age", JsonValue::number(25)),
    ]);
    db.insert("users", &payload2).await.expect("insert");

    let result = db.query("SELECT * FROM users").await.expect("query");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.statement, "select");
    db.close().await.expect("close");
}

#[tokio::test]
async fn bulk_insert_returns_total_affected() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let payloads = vec![
        JsonValue::object([("name", JsonValue::string("a"))]),
        JsonValue::object([("name", JsonValue::string("b"))]),
        JsonValue::object([("name", JsonValue::string("c"))]),
    ];
    let n = db.bulk_insert("items", &payloads).await.expect("bulk");
    assert_eq!(n, 3);
}

#[tokio::test]
async fn bad_sql_returns_query_error() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let err = db.query("NOT A VALID STATEMENT $$$").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::QueryError);
}

#[tokio::test]
async fn grpc_uri_returns_feature_disabled_when_grpc_off() {
    // The default feature set is `embedded` only, so grpc:// should
    // give a clean FEATURE_DISABLED error instead of panicking.
    let result = Reddb::connect("grpc://localhost:50051").await;
    let err = result.unwrap_err();
    assert_eq!(err.code, ErrorCode::FeatureDisabled);
}

#[tokio::test]
async fn json_value_to_json_string_round_trip() {
    let v = JsonValue::object([
        ("name", JsonValue::string("Alice")),
        ("active", JsonValue::bool(true)),
        ("score", JsonValue::number(3.14)),
        (
            "tags",
            JsonValue::array([JsonValue::string("a"), JsonValue::string("b")]),
        ),
    ]);
    let s = v.to_json_string();
    // Order of keys is insertion-preserving for our Vec-backed object.
    assert!(s.contains("\"name\":\"Alice\""));
    assert!(s.contains("\"active\":true"));
    assert!(s.contains("\"tags\":[\"a\",\"b\"]"));
}
