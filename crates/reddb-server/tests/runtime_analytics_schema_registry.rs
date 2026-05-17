//! Issue #577 — Analytics slice 2: AnalyticsSchemaRegistry end-to-end.
//!
//! Drives the registry through the runtime: register a schema, see it
//! survive a re-open of `latest()` / `list()`, then exercise the
//! `red.schema_registry` virtual table over the public SQL surface and
//! confirm the INSERT hook rejects bad payloads.

use reddb_server::runtime::analytics_schema_registry as registry;
use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

const PAGE_VIEW_SCHEMA: &str = r#"{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}"#;

#[test]
fn schema_persisted_and_surfaced_via_virtual_table() {
    let rt = rt();
    let store = rt.db().store();
    let version = registry::register(store.as_ref(), "page_view", PAGE_VIEW_SCHEMA)
        .expect("register page_view");
    assert_eq!(version, 1);
    let (v, schema) = registry::latest(store.as_ref(), "page_view").expect("latest present");
    assert_eq!(v, 1);
    assert!(schema.contains("\"url\""));

    let res = rt
        .execute_query("SELECT event_name, version FROM red.schema_registry")
        .expect("query schema_registry virtual table");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 1, "expected one schema row, got {:?}", rows);
}

#[test]
fn insert_with_valid_payload_lands() {
    let rt = rt();
    let store = rt.db().store();
    registry::register(store.as_ref(), "page_view", PAGE_VIEW_SCHEMA).unwrap();
    rt.execute_query("CREATE TIMESERIES events")
        .expect("create timeseries collection");
    rt.execute_query(
        r#"INSERT INTO events (metric, value, event_name, payload) VALUES ('events', 1.0, 'page_view', '{"url":"/x"}')"#,
    )
    .expect("valid payload accepted");
}

#[test]
fn insert_missing_required_field_rejected() {
    let rt = rt();
    let store = rt.db().store();
    registry::register(store.as_ref(), "page_view", PAGE_VIEW_SCHEMA).unwrap();
    rt.execute_query("CREATE TIMESERIES events")
        .expect("create timeseries collection");
    let err = rt
        .execute_query(
            r#"INSERT INTO events (metric, value, event_name, payload) VALUES ('events', 1.0, 'page_view', '{}')"#,
        )
        .unwrap_err();
    match err {
        RedDBError::InvalidOperation(body) => {
            assert!(
                body.contains("AnalyticsSchemaError:MissingRequiredField"),
                "unexpected body: {body}"
            );
            assert!(body.contains(":url"), "expected field 'url' in body: {body}");
        }
        other => panic!("expected InvalidOperation, got {other:?}"),
    }
}

#[test]
fn insert_unknown_field_rejected() {
    let rt = rt();
    let store = rt.db().store();
    registry::register(store.as_ref(), "page_view", PAGE_VIEW_SCHEMA).unwrap();
    rt.execute_query("CREATE TIMESERIES events").unwrap();
    let err = rt
        .execute_query(
            r#"INSERT INTO events (metric, value, event_name, payload) VALUES ('events', 1.0, 'page_view', '{"url":"/x","mystery":"y"}')"#,
        )
        .unwrap_err();
    match err {
        RedDBError::InvalidOperation(body) => {
            assert!(
                body.contains("AnalyticsSchemaError:UnknownField"),
                "unexpected body: {body}"
            );
        }
        other => panic!("expected InvalidOperation, got {other:?}"),
    }
}

#[test]
fn insert_for_unregistered_event_name_accepted() {
    // Collections without any registered schema for the event_name
    // accept the row as today (back-compat criterion).
    let rt = rt();
    rt.execute_query("CREATE TIMESERIES events").unwrap();
    rt.execute_query(
        r#"INSERT INTO events (metric, value, event_name, payload) VALUES ('events', 1.0, 'never_registered', '{"anything":1}')"#,
    )
    .expect("unregistered event_name should be accepted");
}
