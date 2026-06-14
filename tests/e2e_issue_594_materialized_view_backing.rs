//! Issue #594 — slice 9b of #575.
//!
//! Materialized views get a real Table-shaped backing collection on
//! `CREATE`. The view rewriter is bypassed for materialized views so
//! `SELECT FROM v` resolves to the backing collection rather than
//! re-executing the body. REFRESH still updates the cache slot —
//! wiring REFRESH through the backing is the job of slice 9c.

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn run_with_large_stack(name: &str, f: fn()) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .expect("spawn materialized view backing test")
        .join()
        .expect("materialized view backing test panicked");
}

#[test]
fn create_materialized_view_provisions_empty_backing_collection() {
    run_with_large_stack(
        "create-materialized-view-provisions-empty-backing-collection",
        create_materialized_view_provisions_empty_backing_collection_impl,
    );
}

fn create_materialized_view_provisions_empty_backing_collection_impl() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE orders (id INT, total INT, status TEXT)");
    exec(
        &rt,
        "INSERT INTO orders (id, total, status) VALUES \
           (1, 100, 'paid'), (2, 200, 'paid'), (3, 300, 'pending')",
    );
    exec(
        &rt,
        "CREATE MATERIALIZED VIEW paid_orders AS \
         SELECT * FROM orders WHERE status = 'paid'",
    );

    // The rewriter must skip materialized views: SELECT resolves to
    // the (empty) backing collection rather than re-executing the
    // filtered body. Pre-9c, REFRESH does not populate the backing,
    // so the assertion is "0 rows, no error".
    let result = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(
        result.result.records.len(),
        0,
        "empty backing collection must return 0 rows, not the body's filtered rows"
    );

    // Issue #595 slice 9c — REFRESH now writes through the backing
    // collection, so `current_row_count` scrapes live from the
    // backing (mirrors the slice-10 invariant on `queue_pending_gauge`)
    // and `SELECT FROM v` returns the materialised rows.
    let result = exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
    assert_eq!(result.statement_type, "refresh_materialized_view");
    let count = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "paid_orders")
        .expect("metadata")
        .current_row_count;
    assert_eq!(count, 2, "current_row_count must scrape live backing");

    let result = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(result.result.records.len(), 2);
}

#[test]
fn drop_materialized_view_drops_backing_collection() {
    run_with_large_stack(
        "drop-materialized-view-drops-backing-collection",
        drop_materialized_view_drops_backing_collection_impl,
    );
}

fn drop_materialized_view_drops_backing_collection_impl() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT)");
    exec(&rt, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t");
    // Backing exists — SELECT returns an empty result (not an error).
    let result = exec(&rt, "SELECT * FROM mv");
    assert_eq!(result.result.records.len(), 0);

    exec(&rt, "DROP MATERIALIZED VIEW mv");

    // Backing was dropped — the name is no longer a known relation.
    // Either an error or empty result is acceptable; what we forbid
    // is the rewriter quietly resurrecting the body.
    let res = rt.execute_query("SELECT * FROM mv");
    match res {
        Err(_) => {}
        Ok(r) => assert!(
            r.result.records.is_empty(),
            "post-drop SELECT must not return rows"
        ),
    }
}

#[test]
fn regular_view_rewrite_unchanged_after_slice_9b() {
    run_with_large_stack(
        "regular-view-rewrite-unchanged-after-slice-9b",
        regular_view_rewrite_unchanged_after_slice_9b_impl,
    );
}

fn regular_view_rewrite_unchanged_after_slice_9b_impl() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE users (id INT, active BOOLEAN)");
    exec(
        &rt,
        "INSERT INTO users (id, active) VALUES (1, true), (2, false), (3, true)",
    );
    // Non-materialized view: rewriter still substitutes the body and
    // applies the filter — this is the regression guard for the
    // rewrite-skip predicate (it must trip only for materialized).
    exec(
        &rt,
        "CREATE VIEW active_users AS SELECT id FROM users WHERE active = true",
    );
    let result = exec(&rt, "SELECT * FROM active_users");
    assert_eq!(result.result.records.len(), 2);
}
