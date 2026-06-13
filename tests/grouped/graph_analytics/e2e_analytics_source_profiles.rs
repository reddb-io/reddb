#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use support::{checkpoint_and_reopen, PersistentDbPath};

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn null_or_text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> Option<&'a str> {
    match row.get(field) {
        Some(Value::Null) => None,
        Some(Value::Text(value)) => Some(value.as_ref()),
        other => panic!("expected {field} text or null, got {other:?} in {row:?}"),
    }
}

#[test]
fn analytics_source_profile_persists_and_keeps_backing_collection_writable() {
    let path = PersistentDbPath::new("analytics_source_profile");
    let rt = path.open_runtime();
    exec(
        &rt,
        "CREATE TABLE events (ts INTEGER, event_name TEXT, actor_id TEXT, session_id TEXT, props TEXT)",
    );

    exec(
        &rt,
        "CREATE ANALYTICS SOURCE product_events ON events \
         TIME FIELD ts EVENT FIELD event_name ACTOR FIELD actor_id \
         SESSION FIELD session_id PROPERTIES FIELD props",
    );

    exec(
        &rt,
        "INSERT INTO events (ts, event_name, actor_id, session_id, props) \
         VALUES (1, 'signup', 'user-1', 'sess-1', '{}')",
    );
    let raw_rows = exec(&rt, "SELECT event_name, actor_id FROM events");
    assert_eq!(raw_rows.result.records.len(), 1);

    let source_rows = exec(
        &rt,
        "SELECT name, collection, time_field, event_field, actor_field, session_field, properties_field \
         FROM red.analytics.sources WHERE name = 'product_events'",
    );
    assert_eq!(source_rows.result.records.len(), 1);
    let row = &source_rows.result.records[0];
    assert_eq!(text(row, "name"), "product_events");
    assert_eq!(text(row, "collection"), "events");
    assert_eq!(text(row, "time_field"), "ts");
    assert_eq!(text(row, "event_field"), "event_name");
    assert_eq!(text(row, "actor_field"), "actor_id");
    assert_eq!(null_or_text(row, "session_field"), Some("session_id"));
    assert_eq!(null_or_text(row, "properties_field"), Some("props"));

    let collections = exec(
        &rt,
        "SELECT name FROM red.collections WHERE name = 'product_events'",
    );
    assert!(
        collections.result.records.is_empty(),
        "analytics source profile must not create a raw collection"
    );

    let reopened = checkpoint_and_reopen(&path, rt);
    let persisted = exec(
        &reopened,
        "SELECT collection, time_field, event_field, actor_field, session_field, properties_field \
         FROM red.analytics.sources WHERE name = 'product_events'",
    );
    assert_eq!(persisted.result.records.len(), 1);
    let row = &persisted.result.records[0];
    assert_eq!(text(row, "collection"), "events");
    assert_eq!(text(row, "time_field"), "ts");
    assert_eq!(text(row, "event_field"), "event_name");
    assert_eq!(text(row, "actor_field"), "actor_id");
    assert_eq!(null_or_text(row, "session_field"), Some("session_id"));
    assert_eq!(null_or_text(row, "properties_field"), Some("props"));
}

#[test]
fn analytics_source_profile_validates_backing_collection_and_fields() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(
        &rt,
        "CREATE TABLE events (ts INTEGER, event_name TEXT, actor_id TEXT)",
    );
    exec(&rt, "CREATE TIMESERIES samples RETENTION 1 d");

    for sql in [
        "CREATE ANALYTICS SOURCE missing_source ON missing \
         TIME FIELD ts EVENT FIELD event_name ACTOR FIELD actor_id",
        "CREATE ANALYTICS SOURCE timeseries_source ON samples \
         TIME FIELD ts EVENT FIELD event_name ACTOR FIELD actor_id",
        "CREATE ANALYTICS SOURCE bad_field ON events \
         TIME FIELD missing_ts EVENT FIELD event_name ACTOR FIELD actor_id",
    ] {
        let err = rt
            .execute_query(sql)
            .expect_err(&format!("{sql} should fail"))
            .to_string();
        assert!(
            err.contains("analytics source"),
            "expected clear analytics source error for {sql}, got {err}"
        );
    }
}
