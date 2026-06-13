//! Issue #590 — Analytics slice 7b: end-to-end coverage for the
//! five window-only functions (ROW_NUMBER, RANK, DENSE_RANK, LAG,
//! LEAD) wired through the planner+runtime against in-memory
//! fixtures.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn setup_events() -> RedDBRuntime {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT)".into(),
    })
    .expect("create events table");
    rt
}

fn insert(rt: &RedDBRuntime, id: i64, user: &str, ts: i64) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: format!("INSERT INTO events (id, user_id, ts) VALUES ({id}, '{user}', {ts})"),
        })
        .expect("insert event");
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
    match row
        .get(col)
        .unwrap_or_else(|| panic!("column {col} missing"))
    {
        Value::Integer(v) => *v,
        Value::BigInt(v) => *v,
        Value::UnsignedInteger(v) => *v as i64,
        other => panic!("expected integer {col}, got {other:?}"),
    }
}

fn col_int_opt(row: &reddb::storage::query::unified::UnifiedRecord, col: &str) -> Option<i64> {
    match row.get(col) {
        Some(Value::Null) | None => None,
        Some(Value::Integer(v)) => Some(*v),
        Some(Value::BigInt(v)) => Some(*v),
        Some(Value::UnsignedInteger(v)) => Some(*v as i64),
        Some(other) => panic!("expected integer {col} or NULL, got {other:?}"),
    }
}

#[test]
fn row_number_partitioned_per_user() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 200);
    insert(&rt, 3, "u2", 50);
    insert(&rt, 4, "u1", 150);
    insert(&rt, 5, "u2", 75);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, user_id, ts, \
                    ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY ts) AS rn \
                    FROM events"
                .into(),
        })
        .expect("row_number select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 5);
    let by_id: std::collections::HashMap<i64, i64> =
        rows.iter().map(|r| (id_int(r), col_int(r, "rn"))).collect();
    // u1 sorted by ts: 1 (100), 4 (150), 2 (200)
    assert_eq!(by_id[&1], 1);
    assert_eq!(by_id[&4], 2);
    assert_eq!(by_id[&2], 3);
    // u2 sorted by ts: 3 (50), 5 (75)
    assert_eq!(by_id[&3], 1);
    assert_eq!(by_id[&5], 2);
}

#[test]
fn rank_and_dense_rank_show_tie_gap_difference() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 100); // tied with id 1
    insert(&rt, 3, "u1", 300);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, \
                    RANK() OVER (PARTITION BY user_id ORDER BY ts) AS rk, \
                    DENSE_RANK() OVER (PARTITION BY user_id ORDER BY ts) AS drk \
                    FROM events"
                .into(),
        })
        .expect("rank select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 3);
    let by_id: std::collections::HashMap<i64, (i64, i64)> = rows
        .iter()
        .map(|r| (id_int(r), (col_int(r, "rk"), col_int(r, "drk"))))
        .collect();
    assert_eq!(by_id[&1], (1, 1));
    assert_eq!(by_id[&2], (1, 1));
    // RANK leaves a gap; DENSE_RANK does not.
    assert_eq!(by_id[&3], (3, 2));
}

#[test]
fn lag_returns_prior_ts_per_user_and_null_on_first_row() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 200);
    insert(&rt, 3, "u1", 300);
    insert(&rt, 4, "u2", 50);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, \
                    LAG(ts) OVER (PARTITION BY user_id ORDER BY ts) AS prev_ts \
                    FROM events"
                .into(),
        })
        .expect("lag select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 4);
    let by_id: std::collections::HashMap<i64, Option<i64>> = rows
        .iter()
        .map(|r| (id_int(r), col_int_opt(r, "prev_ts")))
        .collect();
    assert_eq!(by_id[&1], None);
    assert_eq!(by_id[&2], Some(100));
    assert_eq!(by_id[&3], Some(200));
    assert_eq!(by_id[&4], None);
}

#[test]
fn lead_returns_next_ts_or_null_on_last_row() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 200);
    insert(&rt, 3, "u1", 300);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, \
                    LEAD(ts) OVER (PARTITION BY user_id ORDER BY ts) AS next_ts \
                    FROM events"
                .into(),
        })
        .expect("lead select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 3);
    let by_id: std::collections::HashMap<i64, Option<i64>> = rows
        .iter()
        .map(|r| (id_int(r), col_int_opt(r, "next_ts")))
        .collect();
    assert_eq!(by_id[&1], Some(200));
    assert_eq!(by_id[&2], Some(300));
    assert_eq!(by_id[&3], None);
}

#[test]
fn lag_with_offset_and_default_value() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 200);
    insert(&rt, 3, "u1", 300);
    insert(&rt, 4, "u1", 400);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, \
                    LAG(ts, 2, -1) OVER (PARTITION BY user_id ORDER BY ts) AS lag2 \
                    FROM events"
                .into(),
        })
        .expect("lag(offset, default) select");
    let rows = &res.result.records;
    let by_id: std::collections::HashMap<i64, i64> = rows
        .iter()
        .map(|r| (id_int(r), col_int(r, "lag2")))
        .collect();
    // ordered ts: 100, 200, 300, 400 (positions 0,1,2,3); offset=2:
    // 0 → default (-1), 1 → default (-1), 2 → 100, 3 → 200
    assert_eq!(by_id[&1], -1);
    assert_eq!(by_id[&2], -1);
    assert_eq!(by_id[&3], 100);
    assert_eq!(by_id[&4], 200);
}

#[test]
fn mixed_select_window_alongside_columns_does_not_promote_to_aggregate() {
    // Acceptance: a SELECT with a window function plus plain columns
    // (no GROUP BY, no bare aggregate) must execute as a non-aggregate
    // query — every input row produces one output row, the window
    // column has the per-row value, and the plain columns are
    // untouched.
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);
    insert(&rt, 2, "u1", 200);
    insert(&rt, 3, "u2", 50);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, user_id, ts, \
                    ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY ts) AS rn \
                    FROM events"
                .into(),
        })
        .expect("mixed select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 3, "non-aggregate: one row per input event");

    let mut ids: Vec<i64> = rows.iter().map(id_int).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2, 3]);

    // Every row has a non-null rn integer.
    for row in rows {
        match row.get("rn") {
            Some(Value::Integer(_)) | Some(Value::BigInt(_)) => {}
            other => panic!("expected integer rn, got {other:?}"),
        }
    }
}
