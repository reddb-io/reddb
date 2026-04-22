//! End-to-end: HYPERTABLE_PRUNE_CHUNKS scalar — planner primitive
//! exposed over SQL for hypertable chunks.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

const HOUR_NS: u64 = 3_600_000_000_000;

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
    let kept = r.result.records[0].values.get("kept").expect("kept");
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
    let kept = r.result.records[0].values.get("kept").expect("kept");
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
    let kept = r.result.records[0].values.get("kept").expect("kept");
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
    let kept = r.result.records[0].values.get("kept").expect("kept");
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

#[test]
fn prune_unknown_hypertable_returns_null() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT HYPERTABLE_PRUNE_CHUNKS('nope', 0, 1) AS kept".into(),
        })
        .expect("ok");
    let kept = r.result.records[0].values.get("kept").expect("kept");
    assert!(matches!(kept, Value::Null), "expected Null, got {kept:?}");
}
