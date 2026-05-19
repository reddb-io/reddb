//! Issue #591 — Analytics slice 7c: end-to-end coverage for aggregate
//! functions in OVER position (SUM/COUNT/AVG/MIN/MAX) across the
//! three supported frame variants (partition-default, ROWS UNBOUNDED
//! PRECEDING ... CURRENT ROW, ROWS N PRECEDING ... CURRENT ROW), plus
//! the default ordered range frame.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn setup_purchases() -> RedDBRuntime {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT)"
            .into(),
    })
    .expect("create purchases table");
    rt
}

fn insert_purchase(rt: &RedDBRuntime, id: i64, user: &str, ts: i64, amount: i64) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO purchases (id, user_id, ts, amount) VALUES ({id}, '{user}', {ts}, {amount})"
            ),
        })
        .expect("insert purchase");
}

fn id_int(row: &reddb::storage::query::unified::UnifiedRecord) -> i64 {
    match row.get("id").expect("id column") {
        Value::Integer(v) => *v,
        Value::BigInt(v) => *v,
        Value::UnsignedInteger(v) => *v as i64,
        other => panic!("expected integer id, got {other:?}"),
    }
}

fn col_int(row: &reddb::storage::query::unified::UnifiedRecord, col: &str) -> i64 {
    match row.get(col).unwrap_or_else(|| panic!("column {col} missing")) {
        Value::Integer(v) => *v,
        Value::BigInt(v) => *v,
        Value::UnsignedInteger(v) => *v as i64,
        other => panic!("expected integer {col}, got {other:?}"),
    }
}

fn col_f64(row: &reddb::storage::query::unified::UnifiedRecord, col: &str) -> f64 {
    match row.get(col).unwrap_or_else(|| panic!("column {col} missing")) {
        Value::Float(v) => *v,
        Value::Integer(v) | Value::BigInt(v) => *v as f64,
        other => panic!("expected float {col}, got {other:?}"),
    }
}

fn run(rt: &RedDBRuntime, query: &str) -> Vec<reddb::storage::query::unified::UnifiedRecord> {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput { query: query.to_string() })
        .expect("query")
        .result
        .records
}

#[test]
fn sum_partition_by_only_uses_full_partition_as_frame() {
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 20);
    insert_purchase(&rt, 3, "u1", 300, 30);
    insert_purchase(&rt, 4, "u2", 100, 7);
    insert_purchase(&rt, 5, "u2", 200, 9);

    let rows = run(
        &rt,
        "SELECT id, user_id, SUM(amount) OVER (PARTITION BY user_id) AS total \
         FROM purchases",
    );
    assert_eq!(rows.len(), 5);
    let by_id: std::collections::HashMap<i64, i64> =
        rows.iter().map(|r| (id_int(r), col_int(r, "total"))).collect();
    for &id in &[1i64, 2, 3] {
        assert_eq!(by_id[&id], 60, "u1 total");
    }
    for &id in &[4i64, 5] {
        assert_eq!(by_id[&id], 16, "u2 total");
    }
}

#[test]
fn count_avg_min_max_unordered_aggregate_over_partition() {
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 20);
    insert_purchase(&rt, 3, "u1", 300, 30);

    let rows = run(
        &rt,
        "SELECT id, \
            COUNT(*) OVER (PARTITION BY user_id) AS c, \
            AVG(amount) OVER (PARTITION BY user_id) AS a, \
            MIN(amount) OVER (PARTITION BY user_id) AS mn, \
            MAX(amount) OVER (PARTITION BY user_id) AS mx \
         FROM purchases",
    );
    for r in &rows {
        assert_eq!(col_int(r, "c"), 3);
        assert!((col_f64(r, "a") - 20.0).abs() < 1e-9);
        assert_eq!(col_int(r, "mn"), 10);
        assert_eq!(col_int(r, "mx"), 30);
    }
}

#[test]
fn sum_ordered_default_frame_is_running_total() {
    // Acceptance: ORDER BY present, no explicit frame → SQL default
    // RANGE UNBOUNDED PRECEDING AND CURRENT ROW. With distinct ORDER
    // BY keys this collapses to a running total.
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 20);
    insert_purchase(&rt, 3, "u1", 300, 30);

    let rows = run(
        &rt,
        "SELECT id, ts, \
            SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) AS running \
         FROM purchases",
    );
    let by_id: std::collections::HashMap<i64, i64> = rows
        .iter()
        .map(|r| (id_int(r), col_int(r, "running")))
        .collect();
    assert_eq!(by_id[&1], 10);
    assert_eq!(by_id[&2], 30);
    assert_eq!(by_id[&3], 60);
}

#[test]
fn range_default_with_ties_groups_peers() {
    // Frame-default verification: RANGE CURRENT ROW means "all peers"
    // — two rows tied on ts should both see the same running total
    // that includes both of their amounts.
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 100, 5); // tied with id=1
    insert_purchase(&rt, 3, "u1", 200, 30);

    let rows = run(
        &rt,
        "SELECT id, SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) AS running \
         FROM purchases",
    );
    let by_id: std::collections::HashMap<i64, i64> = rows
        .iter()
        .map(|r| (id_int(r), col_int(r, "running")))
        .collect();
    // ts=100 peers: both rows see 10+5 = 15.
    assert_eq!(by_id[&1], 15);
    assert_eq!(by_id[&2], 15);
    // ts=200: 15 + 30 = 45.
    assert_eq!(by_id[&3], 45);
}

#[test]
fn rows_unbounded_preceding_current_row_is_per_row_running() {
    // Same shape as the default ordered frame on distinct keys but
    // explicit ROWS unit — also distinguishes from RANGE on ties:
    // even with tied ORDER BY keys, ROWS only counts physically-prior
    // rows.
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 100, 5); // tied on ts
    insert_purchase(&rt, 3, "u1", 200, 30);

    let rows = run(
        &rt,
        "SELECT id, SUM(amount) OVER (PARTITION BY user_id ORDER BY ts \
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running \
         FROM purchases",
    );
    let by_id: std::collections::HashMap<i64, i64> = rows
        .iter()
        .map(|r| (id_int(r), col_int(r, "running")))
        .collect();
    // ROWS semantics — each row sees only itself + everything
    // physically before. With tied ts the two peers diverge: the
    // first peer sees only itself (5 or 10), the second peer sees
    // both (15). The relative ordering of the two peers under
    // unstable sort is not guaranteed, so accept either pairing.
    let peer_totals: Vec<i64> = vec![by_id[&1], by_id[&2]];
    assert!(peer_totals.contains(&15), "second peer must see both: {peer_totals:?}");
    let first_peer = *peer_totals.iter().find(|&&v| v != 15).expect("first peer");
    assert!(
        first_peer == 5 || first_peer == 10,
        "first peer must see just one row's amount, got {first_peer}"
    );
    assert_eq!(by_id[&3], 45);
}

#[test]
fn rows_n_preceding_current_row_is_trailing_window() {
    // Acceptance: trailing window N=1 (2-row trailing sum) and N=4
    // (5-row trailing avg) both work.
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 20);
    insert_purchase(&rt, 3, "u1", 300, 30);
    insert_purchase(&rt, 4, "u1", 400, 40);
    insert_purchase(&rt, 5, "u1", 500, 50);

    let rows = run(
        &rt,
        "SELECT id, \
            SUM(amount) OVER (PARTITION BY user_id ORDER BY ts \
                ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS sum2, \
            AVG(amount) OVER (PARTITION BY user_id ORDER BY ts \
                ROWS BETWEEN 4 PRECEDING AND CURRENT ROW) AS trailing_avg \
         FROM purchases",
    );
    let by_id: std::collections::HashMap<i64, (i64, f64)> = rows
        .iter()
        .map(|r| (id_int(r), (col_int(r, "sum2"), col_f64(r, "trailing_avg"))))
        .collect();
    // sum2: trailing 2-row sums by ts.
    assert_eq!(by_id[&1].0, 10);
    assert_eq!(by_id[&2].0, 30);
    assert_eq!(by_id[&3].0, 50);
    assert_eq!(by_id[&4].0, 70);
    assert_eq!(by_id[&5].0, 90);
    // trailing_avg: avg over up-to-5 preceding rows incl. current.
    assert!((by_id[&1].1 - 10.0).abs() < 1e-9);
    assert!((by_id[&2].1 - 15.0).abs() < 1e-9);
    assert!((by_id[&3].1 - 20.0).abs() < 1e-9);
    assert!((by_id[&4].1 - 25.0).abs() < 1e-9);
    assert!((by_id[&5].1 - 30.0).abs() < 1e-9);
}

#[test]
fn count_min_max_in_trailing_rows_frame() {
    // Acceptance: COUNT / MIN / MAX combinations across frame variants
    // (each function × frame variant has at least one positive case).
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 40);
    insert_purchase(&rt, 3, "u1", 300, 20);
    insert_purchase(&rt, 4, "u1", 400, 30);

    let rows = run(
        &rt,
        "SELECT id, \
            COUNT(amount) OVER (PARTITION BY user_id ORDER BY ts \
                ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS c2, \
            MIN(amount) OVER (PARTITION BY user_id ORDER BY ts \
                ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS mn2, \
            MAX(amount) OVER (PARTITION BY user_id ORDER BY ts \
                ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS mx2 \
         FROM purchases",
    );
    let by_id: std::collections::HashMap<i64, (i64, i64, i64)> = rows
        .iter()
        .map(|r| {
            (
                id_int(r),
                (col_int(r, "c2"), col_int(r, "mn2"), col_int(r, "mx2")),
            )
        })
        .collect();
    // Trailing 2-row windows of amounts [10,40,20,30]:
    // pos 0: [10] → c=1 mn=10 mx=10
    // pos 1: [10,40] → c=2 mn=10 mx=40
    // pos 2: [40,20] → c=2 mn=20 mx=40
    // pos 3: [20,30] → c=2 mn=20 mx=30
    assert_eq!(by_id[&1], (1, 10, 10));
    assert_eq!(by_id[&2], (2, 10, 40));
    assert_eq!(by_id[&3], (2, 20, 40));
    assert_eq!(by_id[&4], (2, 20, 30));
}

#[test]
fn slice_7b_window_functions_still_work_alongside_aggregate_over() {
    // Regression guard for slice 7b: mixing a 7b ranking function and
    // a 7c aggregate OVER in the same SELECT must execute both
    // correctly and must not collapse the row set.
    let rt = setup_purchases();
    insert_purchase(&rt, 1, "u1", 100, 10);
    insert_purchase(&rt, 2, "u1", 200, 20);
    insert_purchase(&rt, 3, "u1", 300, 30);

    let rows = run(
        &rt,
        "SELECT id, \
            ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY ts) AS rn, \
            SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) AS running \
         FROM purchases",
    );
    assert_eq!(rows.len(), 3);
    let by_id: std::collections::HashMap<i64, (i64, i64)> = rows
        .iter()
        .map(|r| (id_int(r), (col_int(r, "rn"), col_int(r, "running"))))
        .collect();
    assert_eq!(by_id[&1], (1, 10));
    assert_eq!(by_id[&2], (2, 30));
    assert_eq!(by_id[&3], (3, 60));
}
