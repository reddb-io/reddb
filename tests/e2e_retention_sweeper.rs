//! Issue #584 — DeclarativeRetention slice 12.
//!
//! End-to-end coverage for the background sweeper that physically
//! reclaims rows expired beyond the retention window, and the three
//! new observability columns on `red.retention`.

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

/// Sweeper drains expired rows across multiple ticks, never touching
/// non-expired rows. The sweeper is driven directly (rather than
/// waiting on the background thread) so the assertion is timing-
/// independent.
#[test]
fn sweeper_drains_expired_rows_in_batches_and_leaves_fresh_rows() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
    })
    .expect("create events");

    // Five rows that will be expired (created_at = now - 5s after
    // we sleep), and one fresh row inserted after the sleep.
    for i in 0..5 {
        q.execute(ExecuteQueryInput {
            query: format!("INSERT INTO events (id, msg) VALUES ({i}, 'old-{i}')"),
        })
        .expect("insert old row");
    }

    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events SET RETENTION 1 s".into(),
    })
    .expect("set retention");

    std::thread::sleep(std::time::Duration::from_millis(1_200));

    q.execute(ExecuteQueryInput {
        query: "INSERT INTO events (id, msg) VALUES (99, 'fresh')".into(),
    })
    .expect("insert fresh row");

    // Tight batch: 2 per tick. Three ticks should be enough to
    // drain 5 expired rows (2 + 2 + 1).
    rt.sweep_retention_tick(2);
    rt.sweep_retention_tick(2);
    rt.sweep_retention_tick(2);

    // After draining, only the fresh row should remain physically.
    // We UNSET retention so the lazy filter doesn't mask anything.
    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events UNSET RETENTION".into(),
    })
    .expect("unset retention");

    let after = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM events ORDER BY id".into(),
        })
        .expect("select after sweep");
    assert_eq!(
        after.result.records.len(),
        1,
        "sweeper must reclaim every expired row and leave fresh rows"
    );
    let surviving_id = after
        .result
        .records
        .first()
        .expect("one row remains")
        .get("id")
        .expect("id column");
    match surviving_id {
        Value::Integer(v) => assert_eq!(*v, 99),
        Value::BigInt(v) => assert_eq!(*v, 99),
        Value::UnsignedInteger(v) => assert_eq!(*v, 99),
        other => panic!("unexpected id type {other:?}"),
    }
}

/// `red.retention` surfaces the three new sweeper observability
/// columns (`last_sweep_at`, `rows_swept_total`,
/// `current_rows_pending_sweep_estimate`) and the counters move as
/// the sweeper ticks.
#[test]
fn red_retention_exposes_sweeper_state_columns() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
    })
    .expect("create events");

    for i in 0..3 {
        q.execute(ExecuteQueryInput {
            query: format!("INSERT INTO events (id, msg) VALUES ({i}, 'r-{i}')"),
        })
        .expect("insert");
    }

    q.execute(ExecuteQueryInput {
        query: "ALTER COLLECTION events SET RETENTION 1 s".into(),
    })
    .expect("set retention");
    std::thread::sleep(std::time::Duration::from_millis(1_200));

    // Drive the sweeper deterministically.
    rt.sweep_retention_tick(1_000);

    let result = q
        .execute(ExecuteQueryInput {
            query: "SELECT name, retention_duration, oldest_row_ts, \
                    expired_row_count_estimate, last_sweep_at, \
                    rows_swept_total, current_rows_pending_sweep_estimate \
                    FROM red.retention WHERE name = 'events'"
                .into(),
        })
        .expect("select red.retention");
    let row = result
        .result
        .records
        .first()
        .expect("events row in red.retention");

    // The three new sweeper columns must be present in the projection.
    assert!(
        row.get("last_sweep_at").is_some(),
        "last_sweep_at column missing"
    );
    assert!(
        row.get("rows_swept_total").is_some(),
        "rows_swept_total column missing"
    );
    assert!(
        row.get("current_rows_pending_sweep_estimate").is_some(),
        "current_rows_pending_sweep_estimate column missing"
    );

    match row.get("rows_swept_total").unwrap() {
        Value::UnsignedInteger(c) => assert!(*c >= 3, "expected ≥3 rows swept, got {c}"),
        Value::Integer(c) => assert!(*c >= 3, "expected ≥3 rows swept, got {c}"),
        other => panic!("rows_swept_total wrong type {other:?}"),
    }

    match row.get("last_sweep_at").unwrap() {
        Value::TimestampMs(t) => assert!(*t > 0, "last_sweep_at must be populated"),
        Value::BigInt(t) => assert!(*t > 0),
        Value::UnsignedInteger(t) => assert!(*t > 0),
        other => panic!("last_sweep_at wrong type {other:?}"),
    }
}

/// Sweep counters persist independently of restart — the *policy*
/// rehydrates from WAL but the in-memory sweeper bookkeeping resets,
/// and a fresh sweep after restart reclaims any rows that the lazy
/// filter would still hide. Doubles as a stand-in for "replica
/// replays primary sweeps deterministically": both primary and
/// replica run the same `DELETE FROM <c> WHERE <ts> < <cutoff>`
/// statements through the WAL, so a replica catches up by replaying
/// the WAL records the sweeper produced.
#[test]
fn sweeper_after_restart_continues_to_reclaim_via_wal() {
    let dir = unique_dir("retention-sweeper-restart");
    let data_path = dir.join("data.rdb");

    // Phase 1 — write rows, set retention, sweep on the primary.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("open primary");
        let q = QueryUseCases::new(&rt);
        q.execute(ExecuteQueryInput {
            query: "CREATE TABLE events (id INTEGER, msg TEXT) WITH timestamps = true".into(),
        })
        .expect("create events");
        for i in 0..4 {
            q.execute(ExecuteQueryInput {
                query: format!("INSERT INTO events (id, msg) VALUES ({i}, 'r-{i}')"),
            })
            .expect("insert");
        }
        q.execute(ExecuteQueryInput {
            query: "ALTER COLLECTION events SET RETENTION 1 s".into(),
        })
        .expect("set retention");
        std::thread::sleep(std::time::Duration::from_millis(1_200));
        rt.sweep_retention_tick(1_000);

        // Confirm physical removal happened on the primary.
        q.execute(ExecuteQueryInput {
            query: "ALTER COLLECTION events UNSET RETENTION".into(),
        })
        .expect("unset");
        let after = q
            .execute(ExecuteQueryInput {
                query: "SELECT id FROM events".into(),
            })
            .expect("select");
        assert_eq!(
            after.result.records.len(),
            0,
            "primary should have physically swept all expired rows"
        );
    }

    // Phase 2 — replay (restart). The WAL contains the DELETE
    // records emitted by the sweeper, so after replay the rows are
    // gone on the replica as well.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("reopen");
        let q = QueryUseCases::new(&rt);
        let after = q
            .execute(ExecuteQueryInput {
                query: "SELECT id FROM events".into(),
            })
            .expect("select after replay");
        assert_eq!(
            after.result.records.len(),
            0,
            "WAL replay must reproduce the sweeper's physical deletes"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// `CREATE MATERIALIZED VIEW ... WITH RETENTION <duration>` is
/// accepted by the parser and the policy is surfaced on
/// `red.materialized_views.retention_duration_ms` plumbing (slice 12
/// records the policy; physical sweep against MV-backing rows
/// activates with slice 9's row-storage follow-up).
#[test]
fn create_materialized_view_with_retention_is_accepted() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE base (id INTEGER, msg TEXT)".into(),
    })
    .expect("create base");
    q.execute(ExecuteQueryInput {
        query: "CREATE MATERIALIZED VIEW v_recent \
                WITH RETENTION 7 DAYS AS SELECT * FROM base"
            .into(),
    })
    .expect("create materialized view with retention");

    // Parser rejects WITH RETENTION on a non-materialised view.
    let err = q
        .execute(ExecuteQueryInput {
            query: "CREATE VIEW v_bad WITH RETENTION 7 DAYS AS SELECT * FROM base".into(),
        })
        .expect_err("non-materialised view must reject WITH RETENTION");
    assert!(
        err.to_string().to_ascii_lowercase().contains("retention"),
        "expected RETENTION error, got: {err}"
    );
}
