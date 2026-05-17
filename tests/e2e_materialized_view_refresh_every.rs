//! Issue #583 — Analytics slice 10: ContinuousMaterializedView.
//!
//! End-to-end coverage for `CREATE MATERIALIZED VIEW ... REFRESH EVERY`:
//!
//! 1. A scheduled view tick refreshes through the background loop —
//!    `red.materialized_views.last_refresh_at` moves forward and the
//!    duration / row-count columns populate.
//! 2. A refresh whose body references a missing column captures the
//!    error in `last_error` while leaving prior content readable.
//! 3. `DROP MATERIALIZED VIEW` cleans up the scheduled task — no
//!    further refreshes happen, and `red.materialized_views` shows
//!    no rows for the dropped view.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn wait_for<F: Fn() -> bool>(timeout: std::time::Duration, check: F) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    check()
}

#[test]
fn scheduled_refresh_ticks_view_and_updates_red_materialized_views() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE signups (id INTEGER, name TEXT)".into(),
    })
    .expect("create table");

    q.execute(ExecuteQueryInput {
        query: "INSERT INTO signups (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')".into(),
    })
    .expect("insert source rows");

    // Sub-second cadence so the test doesn't drag on. The background
    // scheduler thread polls every ~50ms, so 150ms cadence gives the
    // loop ≥1 full opportunity to fire within a reasonable wait.
    q.execute(ExecuteQueryInput {
        query: "CREATE MATERIALIZED VIEW daily_signups AS \
                SELECT id FROM signups REFRESH EVERY 150 ms"
            .into(),
    })
    .expect("create materialized view");

    // Wait until the scheduler has fired at least once.
    let fired = wait_for(std::time::Duration::from_secs(5), || {
        rt.materialized_view_metadata()
            .iter()
            .any(|m| m.name == "daily_signups" && m.last_refresh_at_ms > 0)
    });
    assert!(fired, "scheduled refresh never fired");

    // red.materialized_views must surface the seven columns and
    // reflect the refresh state.
    let result = q
        .execute(ExecuteQueryInput {
            query: "SELECT name, query_text, refresh_every_ms, last_refresh_at, \
                    last_refresh_duration_ms, last_error, current_row_count \
                    FROM red.materialized_views WHERE name = 'daily_signups'"
                .into(),
        })
        .expect("select from red.materialized_views");
    let row = result
        .result
        .records
        .first()
        .expect("daily_signups row in red.materialized_views");
    match row.get("refresh_every_ms").expect("refresh_every_ms") {
        Value::UnsignedInteger(ms) => assert_eq!(*ms, 150),
        other => panic!("expected UnsignedInteger refresh_every_ms, got {other:?}"),
    }
    match row.get("current_row_count").expect("current_row_count") {
        Value::UnsignedInteger(c) => assert_eq!(*c, 3),
        other => panic!("expected UnsignedInteger current_row_count, got {other:?}"),
    }
    assert!(
        matches!(row.get("last_error").expect("last_error"), Value::Null),
        "scheduled refresh succeeded — last_error must be NULL"
    );
}

#[test]
fn refresh_failure_preserves_prior_content_and_records_error() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE t (id INTEGER)".into(),
    })
    .expect("create source");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO t (id) VALUES (1), (2)".into(),
    })
    .expect("insert");

    q.execute(ExecuteQueryInput {
        query: "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t".into(),
    })
    .expect("create mv");
    // Prime the cache with a successful manual refresh.
    q.execute(ExecuteQueryInput {
        query: "REFRESH MATERIALIZED VIEW mv".into(),
    })
    .expect("initial refresh");

    let prior = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "mv")
        .expect("mv metadata");
    assert_eq!(prior.current_row_count, 2);
    assert!(prior.last_error.is_none());

    // Drop the source — the next refresh body executes against a
    // collection that no longer exists, surfacing an error.
    q.execute(ExecuteQueryInput {
        query: "DROP TABLE t".into(),
    })
    .expect("drop source");

    // Manual refresh now fails. The handler captures the error
    // into `last_error` and returns it to the caller.
    let err = q
        .execute(ExecuteQueryInput {
            query: "REFRESH MATERIALIZED VIEW mv".into(),
        })
        .expect_err("refresh against missing source must error");
    let _ = err.to_string();

    let after = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "mv")
        .expect("mv metadata after failure");
    assert!(
        after.last_error.is_some(),
        "failure must populate last_error"
    );
    // Prior row count is preserved per the acceptance criterion.
    assert_eq!(
        after.current_row_count, 2,
        "prior content must remain readable after refresh failure"
    );
}

#[test]
fn drop_materialized_view_cleans_up_scheduled_task() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE t (id INTEGER)".into(),
    })
    .expect("create");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO t (id) VALUES (1)".into(),
    })
    .expect("insert");
    q.execute(ExecuteQueryInput {
        query: "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t REFRESH EVERY 100 ms".into(),
    })
    .expect("create mv");

    assert!(
        wait_for(std::time::Duration::from_secs(5), || {
            rt.materialized_view_metadata()
                .iter()
                .any(|m| m.name == "mv" && m.last_refresh_at_ms > 0)
        }),
        "scheduler never fired before DROP"
    );

    q.execute(ExecuteQueryInput {
        query: "DROP MATERIALIZED VIEW mv".into(),
    })
    .expect("drop mv");

    // No entry in red.materialized_views for the dropped view — and
    // crucially the scheduler can no longer fire it because the cache
    // slot is gone, so no leaked background work remains.
    let after = rt.materialized_view_metadata();
    assert!(
        after.iter().all(|m| m.name != "mv"),
        "dropped view must not remain in red.materialized_views"
    );

    // Sleep past one cadence to prove no new entry materialises.
    std::thread::sleep(std::time::Duration::from_millis(250));
    let still_gone = rt.materialized_view_metadata();
    assert!(
        still_gone.iter().all(|m| m.name != "mv"),
        "dropped view must not be re-registered by the scheduler"
    );
}
