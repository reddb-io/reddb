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
//! 4. `red.retention` virtual view exposes the seven columns
//!    (four contract columns + three sweeper observability columns
//!    added in slice 12).

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
    // Issue #584 slice 12 — retention now physically reclaims expired
    // rows via the background sweeper (~500ms cadence). The original
    // slice-11 "UNSET reveals previously-hidden rows" semantic only
    // holds within the sweeper window. This test asserts both halves:
    //   1. Inside the sweeper window the lazy-on-scan filter hides
    //      expired rows and UNSET re-exposes them (slice 11).
    //   2. After the sweeper has had a chance to tick (slice 12), the
    //      rows are physically gone and UNSET cannot resurrect them.
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
    })
    .expect("create events table");

    q.execute(ExecuteQueryInput {
        query: "INSERT INTO events (id, msg) VALUES (1, 'hello')".into(),
    })
    .expect("insert row");

    // Retention 1s — wait just long enough for the row to be expired
    // by the lazy filter but *not* long enough for the 500ms sweeper
    // to have its first physical tick after the policy goes live.
    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events SET RETENTION 1 s".into(),
    })
    .expect("set retention");

    std::thread::sleep(std::time::Duration::from_millis(1_100));

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

    // Now drive the sweeper deterministically and assert physical
    // removal — UNSET cannot resurrect rows the sweeper has reclaimed.
    rt.sweep_retention_tick(1_000);

    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events UNSET RETENTION".into(),
    })
    .expect("unset retention");

    let after_sweep = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM events".into(),
        })
        .expect("select after sweep + unset");
    assert_eq!(
        after_sweep.result.records.len(),
        0,
        "background sweeper must physically reclaim expired rows; \
         UNSET after the sweep cannot resurrect them"
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
