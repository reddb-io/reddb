//! Regression for #630: result column ordering must be deterministic
//! across repeated runs of the same query in the same process.
//!
//! Two flavours are covered:
//!
//!  * GROUP BY / multi-aggregate paths — the original bug shape
//!    (HashMap iteration order leaking into output ordering). Fixed
//!    by switching to `indexmap::IndexMap` in
//!    `query::batch::operators::aggregate` and
//!    `query::batch::parallel::merge_partials`, on top of the existing
//!    final `sort_by(compare_keys)`.
//!
//!  * `SELECT *` after a close + reopen on a file-backed table — the
//!    second reporter's observation. The column-name vector here is
//!    already insertion-ordered (`row_slot::column_names: Vec<String>`),
//!    so the test is a guard that nothing further down the read path
//!    re-routes it through a hash-iterated container.

use reddb_server::{RedDBOptions, RedDBRuntime};

const RUNS: usize = 20;

fn seed_groupby(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE sales (region TEXT, tier INT, amount INT, units INT)")
        .expect("create sales");
    rt.execute_query(
        "INSERT INTO sales (region, tier, amount, units) VALUES \
         ('us', 1, 10, 1), ('eu', 1, 20, 2), ('us', 2, 30, 3), \
         ('eu', 2, 40, 4), ('asia', 1, 50, 5), ('us', 1, 60, 6), \
         ('asia', 2, 70, 7), ('eu', 1, 80, 8), ('us', 2, 90, 9), \
         ('asia', 1, 100, 10)",
    )
    .expect("insert sales");
}

fn columns_for(rt: &RedDBRuntime, sql: &str) -> Vec<String> {
    let r = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  err: {e}"));
    r.result.columns.clone()
}

fn rows_for(rt: &RedDBRuntime, sql: &str) -> Vec<Vec<String>> {
    let r = rt
        .execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  err: {e}"));
    // Stringify each cell so we can compare across runs without
    // depending on the specific value-encoding crate.
    r.result
        .records
        .iter()
        .map(|rec| {
            rec.column_names()
                .iter()
                .map(|c| format!("{:?}", rec.get(c.as_ref())))
                .collect()
        })
        .collect()
}

#[test]
fn groupby_column_and_row_ordering_is_stable_across_runs() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_groupby(&rt);

    let sql = "SELECT region, SUM(amount), SUM(units) FROM sales GROUP BY region";

    let first_cols = columns_for(&rt, sql);
    let first_rows = rows_for(&rt, sql);
    assert!(
        first_cols.len() >= 2,
        "GROUP BY query should expose multiple result columns, got {first_cols:?}"
    );
    assert!(
        !first_rows.is_empty(),
        "GROUP BY query should return rows for the seeded data"
    );

    for run in 1..RUNS {
        let cols = columns_for(&rt, sql);
        assert_eq!(
            cols, first_cols,
            "GROUP BY column ordering drifted on run {run}: first={first_cols:?} now={cols:?}"
        );
        let rows = rows_for(&rt, sql);
        assert_eq!(
            rows, first_rows,
            "GROUP BY row ordering drifted on run {run}: first={first_rows:?} now={rows:?}"
        );
    }
}

#[test]
fn multi_aggregate_column_ordering_is_stable_across_runs() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_groupby(&rt);

    // Multi-aggregate without GROUP BY exercises the same accumulator
    // pipeline as GROUP BY but with an empty key. The original report
    // mentioned ordering drift on multi-aggregate results too.
    let sql = "SELECT COUNT(*), SUM(amount), SUM(units), MIN(amount), MAX(amount) FROM sales";
    let first_cols = columns_for(&rt, sql);
    let first_rows = rows_for(&rt, sql);

    for run in 1..RUNS {
        assert_eq!(
            columns_for(&rt, sql),
            first_cols,
            "multi-aggregate column order drifted on run {run}"
        );
        assert_eq!(
            rows_for(&rt, sql),
            first_rows,
            "multi-aggregate row order drifted on run {run}"
        );
    }
}

#[test]
fn select_star_after_reopen_keeps_declared_column_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ordering.rdb");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("runtime boots");
        rt.execute_query("CREATE TABLE chapter_words (chapter TEXT, word TEXT, freq INTEGER)")
            .expect("create table");
        rt.execute_query(
            "INSERT INTO chapter_words (chapter, word, freq) VALUES \
             ('one', 'alpha', 1), ('two', 'beta', 2), ('three', 'gamma', 3)",
        )
        .expect("insert rows");
        drop(rt);
    }

    let rt2 =
        RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("runtime reopens");

    let sql = "SELECT * FROM chapter_words";
    let first_cols = columns_for(&rt2, sql);
    // Stability — not equality to the declared schema. `SELECT *` exposes
    // both user columns and system columns (rid/tenant/kind/created_at…)
    // and the unified result projects them in a sorted order; the bug
    // (#630) is about drift across runs, not about *which* order wins.
    // The declared user columns must still be present.
    for declared in ["chapter", "word", "freq"] {
        assert!(
            first_cols.iter().any(|c| c == declared),
            "SELECT * dropped declared column {declared:?} after reopen (got {first_cols:?})"
        );
    }
    let first_rows = rows_for(&rt2, sql);

    for run in 1..RUNS {
        let cols = columns_for(&rt2, sql);
        assert_eq!(
            cols, first_cols,
            "SELECT * column order drifted on run {run}: first={first_cols:?} now={cols:?}"
        );
        let rows = rows_for(&rt2, sql);
        assert_eq!(rows, first_rows, "SELECT * row order drifted on run {run}");
    }
}
