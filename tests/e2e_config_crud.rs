use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn field<'a>(row: &'a reddb::storage::query::unified::UnifiedRecord, name: &str) -> &'a Value {
    row.get(name)
        .unwrap_or_else(|| panic!("missing field {name}: {row:?}"))
}

fn text(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> String {
    match field(row, name) {
        Value::Text(value) => value.to_string(),
        other => panic!("expected text field {name}, got {other:?}"),
    }
}

fn integer(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> i64 {
    match field(row, name) {
        Value::Integer(value) => *value,
        other => panic!("expected integer field {name}, got {other:?}"),
    }
}

fn boolean(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> bool {
    match field(row, name) {
        Value::Boolean(value) => *value,
        other => panic!("expected boolean field {name}, got {other:?}"),
    }
}

#[test]
fn config_create_update_rotate_history_and_tombstone_delete() {
    let rt = rt();

    let created = rt
        .execute_query("PUT CONFIG app_settings theme = 'dark'")
        .expect("put config");
    let row = &created.result.records[0];
    assert_eq!(text(row, "collection"), "app_settings");
    assert_eq!(text(row, "key"), "theme");
    assert_eq!(integer(row, "version"), 1);

    let updated = rt
        .execute_query("PUT CONFIG app_settings theme = 'light'")
        .expect("update config");
    assert_eq!(integer(&updated.result.records[0], "version"), 2);

    let rotated = rt
        .execute_query("ROTATE CONFIG app_settings theme = 'blue'")
        .expect("rotate config");
    assert_eq!(integer(&rotated.result.records[0], "version"), 3);

    let current = rt
        .execute_query("GET CONFIG app_settings theme")
        .expect("get config");
    let row = &current.result.records[0];
    assert_eq!(field(row, "value"), &Value::text("blue"));
    assert_eq!(integer(row, "version"), 3);
    assert!(!boolean(row, "tombstone"));
    assert_eq!(field(row, "tags"), &Value::Null);

    let history = rt
        .execute_query("HISTORY CONFIG app_settings theme")
        .expect("history config");
    assert_eq!(history.result.records.len(), 3);
    assert_eq!(integer(&history.result.records[0], "version"), 1);
    assert_eq!(
        field(&history.result.records[0], "value"),
        &Value::text("dark")
    );
    assert_eq!(integer(&history.result.records[2], "version"), 3);
    assert_eq!(
        field(&history.result.records[2], "value"),
        &Value::text("blue")
    );

    let deleted = rt
        .execute_query("DELETE CONFIG app_settings theme")
        .expect("delete config");
    assert_eq!(integer(&deleted.result.records[0], "version"), 4);

    let current = rt
        .execute_query("GET CONFIG app_settings theme")
        .expect("get tombstone");
    let row = &current.result.records[0];
    assert_eq!(field(row, "value"), &Value::Null);
    assert_eq!(integer(row, "version"), 4);
    assert!(boolean(row, "tombstone"));

    let history = rt
        .execute_query("HISTORY CONFIG app_settings theme")
        .expect("history after delete");
    assert_eq!(history.result.records.len(), 4);
    let tombstone = &history.result.records[3];
    assert_eq!(integer(tombstone, "version"), 4);
    assert_eq!(field(tombstone, "value"), &Value::Null);
    assert!(boolean(tombstone, "tombstone"));
    assert_eq!(text(tombstone, "op"), "delete");
}

#[test]
fn config_rejects_kv_only_volatility_operations() {
    let rt = rt();

    for sql in [
        "PUT CONFIG app_settings ttl_key = 'v' EXPIRE 1 s",
        "ROTATE CONFIG app_settings ttl_key = 'v2' TTL 10",
        "INCR CONFIG app_settings counter",
        "DECR CONFIG app_settings counter",
        "ADD CONFIG app_settings member",
        "INVALIDATE CONFIG app_settings ttl_key",
    ] {
        let err = rt.execute_query(sql).expect_err(sql).to_string();
        assert!(err.contains("INVALID_OPERATION"), "{sql}: {err}");
    }
}

#[test]
fn config_history_is_bounded() {
    let rt = rt();

    for version in 1..=18 {
        rt.execute_query(&format!("ROTATE CONFIG app_settings bounded = {version}"))
            .expect("rotate config");
    }

    let history = rt
        .execute_query("HISTORY CONFIG app_settings bounded")
        .expect("history config");
    assert_eq!(history.result.records.len(), 16);
    assert_eq!(integer(&history.result.records[0], "version"), 3);
    assert_eq!(integer(&history.result.records[15], "version"), 18);
}
