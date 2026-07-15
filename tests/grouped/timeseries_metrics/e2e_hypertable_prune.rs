//! End-to-end: HYPERTABLE_PRUNE_CHUNKS scalar — planner primitive
//! exposed over SQL for hypertable chunks.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

const HOUR_NS: u64 = 3_600_000_000_000;
const GEOFENCE_POLYGON: &str =
    "POLYGON((38.70 -77.20), (38.82 -77.20), (38.82 -77.05), (38.70 -77.05))";

fn explain_ops(rt: &RedDBRuntime, query: &str) -> Vec<String> {
    rt.execute_query(query)
        .expect("EXPLAIN executes")
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("op") {
            Some(Value::Text(op)) => Some(op.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn prune_chunks_returns_overlapping_window() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    // Allocate 3 chunks by routing rows at hours 0, 1, 2 (ns).
    // Runtime INSERT chunk routing isn't wired yet — the pruner only
    // cares about what the registry knows, so we call route() directly
    // through the public API.
    let db = rt.db();
    let reg = db.hypertables();
    reg.route("metrics", 0).expect("route 0");
    reg.route("metrics", HOUR_NS).expect("route 1");
    reg.route("metrics", 2 * HOUR_NS).expect("route 2");
    assert_eq!(reg.show_chunks("metrics").len(), 3);

    // Prune to the window [HOUR_NS, 2*HOUR_NS) — exactly one chunk
    // overlaps (the one starting at HOUR_NS).
    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_PRUNE_CHUNKS('metrics', {lo}, {hi}) AS kept",
                lo = HOUR_NS,
                hi = 2 * HOUR_NS,
            ),
        })
        .expect("prune ok");
    let kept = r.result.records[0].get("kept").expect("kept");
    let arr = match kept {
        Value::Array(v) => v,
        other => panic!("expected Array, got {other:?}"),
    };
    assert_eq!(arr.len(), 1, "one overlapping chunk, got {arr:?}");
}

#[test]
fn prune_wide_window_keeps_everything() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    let db = rt.db();
    let reg = db.hypertables();
    reg.route("metrics", 0);
    reg.route("metrics", HOUR_NS);
    reg.route("metrics", 2 * HOUR_NS);
    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_PRUNE_CHUNKS('metrics', 0, {hi}) AS kept",
                hi = 100 * HOUR_NS,
            ),
        })
        .expect("ok");
    let kept = r.result.records[0].get("kept").expect("kept");
    match kept {
        Value::Array(v) => assert_eq!(v.len(), 3),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn prune_narrow_window_keeps_nothing() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    let db = rt.db();
    let reg = db.hypertables();
    reg.route("metrics", 0);
    reg.route("metrics", HOUR_NS);
    // Window starts far in the future — no chunk should overlap.
    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_PRUNE_CHUNKS('metrics', {lo}, {hi}) AS kept",
                lo = 100 * HOUR_NS,
                hi = 200 * HOUR_NS,
            ),
        })
        .expect("ok");
    let kept = r.result.records[0].get("kept").expect("kept");
    match kept {
        Value::Array(v) => assert!(v.is_empty(), "expected empty, got {v:?}"),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn prune_after_real_inserts_returns_expected_chunk() {
    // End-to-end: CREATE HYPERTABLE, INSERT rows via SQL (routing
    // happens automatically in execute_insert), then consult the
    // pruner over SQL and verify only the overlapping chunk lands.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    for hour in 0..3u64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, {}.0)",
                hour * HOUR_NS,
                hour + 1
            ),
        })
        .expect("insert");
    }

    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_PRUNE_CHUNKS('metrics', {lo}, {hi}) AS kept",
                lo = HOUR_NS,
                hi = 2 * HOUR_NS,
            ),
        })
        .expect("ok");
    let kept = r.result.records[0].get("kept").expect("kept");
    match kept {
        Value::Array(v) => assert_eq!(v.len(), 1, "one overlapping chunk, got {v:?}"),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn insert_routes_rows_into_chunks_automatically() {
    // Proves the INSERT-time routing hook: new hypertable, insert
    // rows at three different hours, verify three chunks exist
    // without touching HypertableRegistry::route directly.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    // Insert rows at ts = 0, ts = 1h, ts = 2h.
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, load) VALUES (0, 1.0)".into(),
    })
    .expect("ins1");
    q.execute(ExecuteQueryInput {
        query: format!("INSERT INTO metrics (ts, load) VALUES ({}, 2.0)", HOUR_NS),
    })
    .expect("ins2");
    q.execute(ExecuteQueryInput {
        query: format!(
            "INSERT INTO metrics (ts, load) VALUES ({}, 3.0)",
            2 * HOUR_NS
        ),
    })
    .expect("ins3");

    let db = rt.db();
    let chunks = db.hypertables().show_chunks("metrics");
    assert_eq!(
        chunks.len(),
        3,
        "expected 3 chunks auto-allocated by INSERT, got {chunks:?}"
    );
}

fn ts_values(records: &[reddb::storage::query::UnifiedRecord], col: &str) -> Vec<i64> {
    let mut out: Vec<i64> = records
        .iter()
        .filter_map(|r| match r.get(col) {
            Some(Value::Integer(n)) => Some(*n),
            Some(Value::UnsignedInteger(n)) => Some(*n as i64),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn select_temporal_predicate_matches_unpruned_full_scan() {
    // Acceptance #2: a temporal range predicate on a hypertable returns
    // exactly the same rows the full scan would. The chunk-pruning hook
    // must not drop a single matching row.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    // Insert one row per hour across 6 hours → 6 chunks.
    for hour in 0..6u64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, {}.0)",
                hour * HOUR_NS,
                hour + 1
            ),
        })
        .expect("insert");
    }

    // Pruned query: ts in [2h, 4h).
    let lo = 2 * HOUR_NS;
    let hi = 4 * HOUR_NS;
    let pruned = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT ts FROM metrics WHERE ts >= {lo} AND ts < {hi}"),
        })
        .expect("pruned select");

    // Reference: the same predicate evaluated client-side over a full
    // unfiltered scan.
    let full = q
        .execute(ExecuteQueryInput {
            query: "SELECT ts FROM metrics".into(),
        })
        .expect("full select");

    let pruned_ts = ts_values(&pruned.result.records, "ts");
    let expected: Vec<i64> = ts_values(&full.result.records, "ts")
        .into_iter()
        .filter(|t| *t >= lo as i64 && *t < hi as i64)
        .collect();
    assert_eq!(
        pruned_ts, expected,
        "pruned rows must equal filtered full scan"
    );
    assert_eq!(pruned_ts, vec![(2 * HOUR_NS) as i64, (3 * HOUR_NS) as i64]);
}

#[test]
fn geo_within_time_range_matches_unindexed_timeseries_scan() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE vehicle_positions TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    for (vehicle, hour, location) in [
        ("van-1", 0, r#"{"lat":38.75,"lon":-77.15}"#),
        ("van-2", 1, r#"{"lat":38.79,"lon":-77.19}"#),
        ("van-3", 2, r#"{"lat":38.75,"lon":-77.10}"#),
        ("van-4", 2, r#"{"lat":38.69,"lon":-77.15}"#),
        ("van-5", 3, r#"{"lat":39.00,"lon":-77.15}"#),
    ] {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO vehicle_positions (metric, value, tags, ts, location) \
                 VALUES ('position', 1.0, '{{\"vehicle\":\"{vehicle}\"}}', {}, JSON_PARSE('{location}'))",
                hour * HOUR_NS
            ),
        })
        .expect("insert position point");
    }

    let lo = HOUR_NS;
    let hi = 3 * HOUR_NS;
    let query = format!(
        "SELECT ts FROM vehicle_positions \
         WHERE GEO_WITHIN(location, {GEOFENCE_POLYGON}) \
           AND ts >= {lo} AND ts < {hi} \
         ORDER BY ts"
    );

    let unindexed = q
        .execute(ExecuteQueryInput {
            query: query.clone(),
        })
        .expect("unindexed scan query");

    q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_vehicle_location ON vehicle_positions (location) USING H3 (9)"
            .into(),
    })
    .expect("create h3 index");

    let indexed = q
        .execute(ExecuteQueryInput { query })
        .expect("indexed query");

    let unindexed_ts = ts_values(&unindexed.result.records, "ts");
    let indexed_ts = ts_values(&indexed.result.records, "ts");
    assert_eq!(
        indexed_ts, unindexed_ts,
        "H3-accelerated timeseries GEO_WITHIN must match the unindexed scan"
    );
    assert_eq!(
        indexed_ts,
        vec![HOUR_NS as i64, (2 * HOUR_NS) as i64],
        "time and geofence predicates must both contribute to the result"
    );
}

#[test]
fn geo_within_time_range_explain_shows_spatial_and_chunk_pruning() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE vehicle_positions TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    for hour in 0..4u64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO vehicle_positions (metric, value, tags, ts, location) \
                 VALUES ('position', 1.0, '{{\"vehicle\":\"v{hour}\"}}', {}, \
                         JSON_PARSE('{{\"lat\":38.75,\"lon\":-77.15}}'))",
                hour * HOUR_NS
            ),
        })
        .expect("insert point");
    }
    q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_vehicle_location ON vehicle_positions (location) USING H3 (9)"
            .into(),
    })
    .expect("create h3 index");

    let ops = explain_ops(
        &rt,
        &format!(
            "EXPLAIN SELECT ts FROM vehicle_positions \
             WHERE GEO_WITHIN(location, {GEOFENCE_POLYGON}) \
               AND ts >= {lo} AND ts < {hi}",
            lo = HOUR_NS,
            hi = 3 * HOUR_NS
        ),
    );

    assert!(
        ops.iter().any(|op| op == "geo_h3_index_seek"),
        "composed query must expose spatial index use: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| op == "hypertable_chunk_prune"),
        "composed query must expose time chunk pruning: {ops:?}"
    );
}

#[test]
fn select_window_outside_all_chunks_returns_empty() {
    // Acceptance #1: a temporal predicate whose window overlaps no chunk
    // prunes every chunk at plan time and returns no rows.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    for hour in 0..3u64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, 1.0)",
                hour * HOUR_NS
            ),
        })
        .expect("insert");
    }
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT ts FROM metrics WHERE ts >= {}", 100 * HOUR_NS),
        })
        .expect("select");
    assert!(
        r.result.records.is_empty(),
        "window past every chunk must prune to empty, got {:?}",
        r.result.records
    );
}

#[test]
fn select_without_temporal_predicate_is_unaffected() {
    // Acceptance #4: a query with no temporal predicate sees every row —
    // pruning must not engage when the WHERE doesn't constrain time.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    for hour in 0..4u64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, {}.0)",
                hour * HOUR_NS,
                hour + 1
            ),
        })
        .expect("insert");
    }
    // Filter on a non-time column: every chunk must stay live.
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT ts FROM metrics WHERE load >= 1.0".into(),
        })
        .expect("select");
    assert_eq!(ts_values(&r.result.records, "ts").len(), 4);
}

#[test]
fn select_on_non_hypertable_collection_is_unaffected() {
    // Acceptance #4: a plain (non-hypertable) collection never enters the
    // pruning branch.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INT, ts INT, label TEXT)".into(),
    })
    .expect("create table");
    for i in 0..3i64 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO events (id, ts, label) VALUES ({}, {}, 'a')",
                i,
                i * 10
            ),
        })
        .expect("insert");
    }
    // A temporal-looking predicate on a plain table must behave exactly
    // like any other range scan — the hypertable pruning branch is never
    // reached because `events` is not registered as a hypertable.
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM events WHERE ts BETWEEN 10 AND 100".into(),
        })
        .expect("select");
    assert_eq!(
        r.result.records.len(),
        2,
        "plain-table range scan unaffected, got {:?}",
        r.result.records
    );
}

#[test]
fn prune_unknown_hypertable_returns_null() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT HYPERTABLE_PRUNE_CHUNKS('nope', 0, 1) AS kept".into(),
        })
        .expect("ok");
    let kept = r.result.records[0].get("kept").expect("kept");
    assert!(matches!(kept, Value::Null), "expected Null, got {kept:?}");
}
