//! Issue #576 slice 1 — CREATE TIMESERIES ... WITH SESSION_KEY <col>
//! SESSION_GAP <duration> persists the pair on the collection contract
//! and surfaces it through the catalog descriptor (and `red.collections`)
//! across an engine restart.

use reddb::application::ExecuteQueryInput;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};
use std::path::PathBuf;

fn unique_dir(prefix: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("reddb-{prefix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn session_clause_persists_across_restart() {
    let dir = unique_dir("timeseries-session");
    let data_path = dir.join("data.rdb");

    // First boot — create a timeseries with the WITH clause and force a
    // metadata flush so the contract is durable before the runtime
    // drops.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("open persistent runtime");
        let q = QueryUseCases::new(&rt);
        q.execute(ExecuteQueryInput {
            query: "CREATE TIMESERIES events WITH SESSION_KEY user_id SESSION_GAP 30 m".into(),
        })
        .expect("create timeseries with session clause");

        let snapshot = rt.db().catalog_model_snapshot();
        let descriptor = snapshot
            .collections
            .iter()
            .find(|c| c.name == "events")
            .expect("events descriptor present after create");
        assert_eq!(descriptor.session_key.as_deref(), Some("user_id"));
        assert_eq!(descriptor.session_gap_ms, Some(30 * 60_000));

        // `execute_create_timeseries` already calls `persist_metadata`
        // at the end of the DDL path, so the contract is durable by
        // the time the runtime drops here.
    }

    // Second boot — descriptor is rehydrated from the persisted
    // contract; both fields survive the restart.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("reopen persistent runtime");
        let snapshot = rt.db().catalog_model_snapshot();
        let descriptor = snapshot
            .collections
            .iter()
            .find(|c| c.name == "events")
            .expect("events descriptor present after restart");
        assert_eq!(descriptor.session_key.as_deref(), Some("user_id"));
        assert_eq!(descriptor.session_gap_ms, Some(30 * 60_000));

        // The runtime view materialised through `red.collections`
        // exposes the same values — proves the surface that the
        // demoable SELECT targets is wired end-to-end.
        let q = QueryUseCases::new(&rt);
        let result = q
            .execute(ExecuteQueryInput {
                query: "SELECT model, session_key, session_gap_ms \
                        FROM red.collections WHERE name = 'events'"
                    .into(),
            })
            .expect("select from red.collections");
        let row = result
            .result
            .records
            .first()
            .expect("at least one row for events");
        use reddb::storage::schema::Value;
        match row.get("session_key").expect("session_key column") {
            Value::Text(s) => assert_eq!(&**s, "user_id"),
            other => panic!("expected Text, got {other:?}"),
        }
        match row.get("session_gap_ms").expect("session_gap_ms column") {
            Value::UnsignedInteger(ms) => assert_eq!(*ms, 30 * 60_000),
            Value::Integer(ms) => assert_eq!(*ms, (30 * 60_000) as i64),
            other => panic!("expected integer, got {other:?}"),
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeseries_without_clause_keeps_session_fields_null() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TIMESERIES bare RETENTION 1 d".into(),
    })
    .expect("create plain timeseries");
    let snapshot = rt.db().catalog_model_snapshot();
    let descriptor = snapshot
        .collections
        .iter()
        .find(|c| c.name == "bare")
        .expect("descriptor present");
    assert!(descriptor.session_key.is_none());
    assert!(descriptor.session_gap_ms.is_none());
}
