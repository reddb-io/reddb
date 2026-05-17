//! Issue #580 — DeclarativeRetention slice 1.
//!
//! End-to-end coverage for `ALTER COLLECTION ... SET RETENTION` /
//! `UNSET RETENTION`:
//!
//! 1. Lazy-on-scan filter — SELECT returns only fresh rows once
//!    retention is set; UNSET re-exposes the previously-hidden rows
//!    (no physical drop happened).
//! 2. Setting retention on a collection without a timestamp column
//!    returns a typed error at ALTER time.
//! 3. WAL replay — restart of a persistent engine rehydrates the
//!    `retention_duration_ms` policy on the descriptor.
//! 4. `red.retention` virtual view exposes the four columns.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
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
fn lazy_filter_drops_expired_then_unset_reveals_them_again() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    // The collection uses the auto `created_at` system column, which
    // the retention filter keys off when `WITH timestamps = true`.
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
    })
    .expect("create events table");

    q.execute(ExecuteQueryInput {
        query: "INSERT INTO events (id, msg) VALUES (1, 'hello')".into(),
    })
    .expect("insert row");

    // Set retention to 1 second, then wait long enough that the row's
    // engine-stamped `created_at` becomes "older than now - 1s".
    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events SET RETENTION 1 s".into(),
    })
    .expect("set retention");

    std::thread::sleep(std::time::Duration::from_millis(1_500));

    let after = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM events".into(),
        })
        .expect("select after retention set");
    assert_eq!(
        after.result.records.len(),
        0,
        "expired rows should be hidden by the lazy-on-scan filter"
    );

    // UNSET — the same row materialises again because the slice
    // never physically dropped it.
    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events UNSET RETENTION".into(),
    })
    .expect("unset retention");

    let revived = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM events".into(),
        })
        .expect("select after unset");
    assert_eq!(
        revived.result.records.len(),
        1,
        "UNSET RETENTION must re-expose previously-hidden rows"
    );
}

#[test]
fn alter_set_retention_without_timestamp_column_is_rejected() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE plain (id INTEGER, name TEXT)".into(),
    })
    .expect("create plain table");
    let err = q
        .execute(ExecuteQueryInput {
            query: "ALTER COLLECTION plain SET RETENTION 7 DAYS".into(),
        })
        .expect_err("retention without timestamp column must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no timestamp column"),
        "expected typed timestamp error, got {msg}"
    );
}

#[test]
fn retention_policy_persists_across_restart() {
    let dir = unique_dir("retention-policy");
    let data_path = dir.join("data.rdb");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("open persistent runtime");
        let q = QueryUseCases::new(&rt);
        q.execute(ExecuteQueryInput {
            query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
        })
        .expect("create events");
        q.execute(ExecuteQueryInput {
            query: "ALTER COLLECTION events SET RETENTION 7 DAYS".into(),
        })
        .expect("set retention");
        let snapshot = rt.db().catalog_model_snapshot();
        let desc = snapshot
            .collections
            .iter()
            .find(|c| c.name == "events")
            .expect("events descriptor before restart");
        assert_eq!(desc.retention_duration_ms, Some(7 * 86_400_000));
    }

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("reopen persistent runtime");
        let snapshot = rt.db().catalog_model_snapshot();
        let desc = snapshot
            .collections
            .iter()
            .find(|c| c.name == "events")
            .expect("events descriptor after restart");
        assert_eq!(
            desc.retention_duration_ms,
            Some(7 * 86_400_000),
            "WAL replay must rehydrate the retention policy"
        );

        // `red.retention` exposes the four contract columns.
        let q = QueryUseCases::new(&rt);
        let result = q
            .execute(ExecuteQueryInput {
                query: "SELECT name, retention_duration, oldest_row_ts, \
                        expired_row_count_estimate \
                        FROM red.retention WHERE name = 'events'"
                    .into(),
            })
            .expect("select from red.retention");
        let row = result
            .result
            .records
            .first()
            .expect("events row in red.retention");
        match row.get("retention_duration").expect("retention_duration") {
            Value::UnsignedInteger(ms) => assert_eq!(*ms, 7 * 86_400_000),
            Value::Integer(ms) => assert_eq!(*ms, (7 * 86_400_000) as i64),
            other => panic!("expected integer retention_duration, got {other:?}"),
        }
        match row
            .get("expired_row_count_estimate")
            .expect("expired_row_count_estimate")
        {
            Value::UnsignedInteger(c) => assert!(*c < u64::MAX),
            Value::Integer(c) => assert!(*c >= 0),
            other => panic!("expected integer estimate, got {other:?}"),
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
