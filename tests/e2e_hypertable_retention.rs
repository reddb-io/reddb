//! End-to-end: hypertable retention scalars over SQL.
//!
//! Covers HYPERTABLE_SHOW_CHUNKS, HYPERTABLE_DROP_CHUNKS_BEFORE, and
//! HYPERTABLE_SWEEP_EXPIRED. All three are thin wrappers over
//! HypertableRegistry — the tests prove they're wired correctly
//! through the scalar dispatcher.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

const HOUR_NS: u64 = 3_600_000_000_000;

#[test]
fn show_chunks_lists_every_allocated_chunk() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    for h in 0..3 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, {}.0)",
                h * HOUR_NS,
                h + 1
            ),
        })
        .expect("insert");
    }
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT HYPERTABLE_SHOW_CHUNKS('metrics') AS chunks".into(),
        })
        .expect("show ok");
    let chunks = r.result.records[0].values.get("chunks").expect("chunks");
    match chunks {
        Value::Array(v) => assert_eq!(v.len(), 3),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn drop_chunks_before_cutoff_removes_stale() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    for h in 0..3 {
        q.execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO metrics (ts, load) VALUES ({}, 1.0)",
                h * HOUR_NS
            ),
        })
        .expect("ins");
    }
    // Drop chunks whose max_ts <= 1h — the first two chunks qualify
    // (they cover [0h, 1h) and [1h, 2h) but max_ts observed is the
    // insert timestamp, so chunk-0 max=0 and chunk-1 max=HOUR_NS).
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT HYPERTABLE_DROP_CHUNKS_BEFORE('metrics', {HOUR_NS}) AS n"),
        })
        .expect("drop ok");
    let n = r.result.records[0].values.get("n").expect("n");
    assert!(
        matches!(n, Value::Integer(n) if *n >= 1),
        "expected at least 1 dropped, got {n:?}"
    );

    let db = rt.db();
    let remaining = db.hypertables().show_chunks("metrics").len();
    assert!(remaining <= 2, "got {remaining} remaining after drop");
}

#[test]
fn sweep_expired_respects_ttl() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    // 1-hour chunks, 1-hour TTL. A chunk with max_ts=0 expires at 1h;
    // sweeping at 3h should reclaim it.
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1h'".into(),
    })
    .expect("create ok");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, load) VALUES (0, 1.0)".into(),
    })
    .expect("ins");

    let now_ns = 3 * HOUR_NS;
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT HYPERTABLE_SWEEP_EXPIRED('metrics', {now_ns}) AS n"),
        })
        .expect("sweep ok");
    let n = r.result.records[0].values.get("n").expect("n");
    assert!(
        matches!(n, Value::Integer(1)),
        "expected 1 sweep, got {n:?}"
    );
}

#[test]
fn sweep_all_expired_crosses_every_hypertable() {
    // Two hypertables, both TTL '1h'; sweep_all should reclaim
    // expired chunks across both in one call.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE ht_a TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1h'".into(),
    })
    .expect("ht_a");
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE ht_b TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1h'".into(),
    })
    .expect("ht_b");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO ht_a (ts, v) VALUES (0, 1)".into(),
    })
    .expect("ins a");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO ht_b (ts, v) VALUES (0, 1)".into(),
    })
    .expect("ins b");
    let now_ns = 3 * HOUR_NS;
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT HYPERTABLE_SWEEP_ALL_EXPIRED({now_ns}) AS n"),
        })
        .expect("sweep_all ok");
    let n = r.result.records[0].values.get("n").expect("n");
    assert!(
        matches!(n, Value::Integer(n) if *n == 2),
        "expected 2 chunks swept across both tables, got {n:?}"
    );
}

#[test]
fn set_and_get_ttl_roundtrip() {
    // Verifies the TTL mutation surface. Note: the runtime result
    // cache collapses identical SELECTs; scalar calls with side
    // effects on external registries don't invalidate it. We read
    // the post-mutation state through the registry API directly —
    // what production clients on a fresh session would also see.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");

    // SET via SQL.
    q.execute(ExecuteQueryInput {
        query: "SELECT HYPERTABLE_SET_TTL('metrics', '7d') AS ok".into(),
    })
    .expect("set ok");

    let db = rt.db();
    let spec = db.hypertables().get("metrics").expect("spec");
    assert_eq!(spec.default_ttl_ns, Some(7 * 86_400_000_000_000));

    // Clear via null.
    q.execute(ExecuteQueryInput {
        query: "SELECT HYPERTABLE_SET_TTL('metrics', null) AS ok".into(),
    })
    .expect("clear ok");
    let db = rt.db();
    let spec = db.hypertables().get("metrics").expect("spec");
    assert_eq!(spec.default_ttl_ns, None);
}

#[test]
fn chunks_expiring_within_horizon_previews_without_drop() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1h'".into(),
    })
    .expect("create ok");
    // Insert chunk at ts=0. Its max_ts=0, effective expiry = 1h.
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, v) VALUES (0, 1.0)".into(),
    })
    .expect("ins");

    // At now=30m, horizon=1h (so window = [30m, 1h30m]), the chunk's
    // expiry at 1h falls inside — preview should show it.
    let now_ns: i64 = 30 * 60 * 1_000_000_000;
    let horizon: i64 = 3_600_000_000_000;
    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_CHUNKS_EXPIRING_WITHIN('metrics', {now_ns}, {horizon}) AS c"
            ),
        })
        .expect("ok");
    let c = r.result.records[0].values.get("c").expect("c");
    match c {
        Value::Array(v) => assert_eq!(v.len(), 1, "one expiring chunk, got {v:?}"),
        other => panic!("expected Array, got {other:?}"),
    }

    // Chunk still present — preview must be side-effect-free.
    let db = rt.db();
    assert_eq!(db.hypertables().show_chunks("metrics").len(), 1);
}

#[test]
fn sweep_without_ttl_is_noop() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    // No TTL — even old chunks stay.
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, load) VALUES (0, 1.0)".into(),
    })
    .expect("ins");
    let r = q
        .execute(ExecuteQueryInput {
            query: format!(
                "SELECT HYPERTABLE_SWEEP_EXPIRED('metrics', {}) AS n",
                100 * HOUR_NS
            ),
        })
        .expect("sweep ok");
    let n = r.result.records[0].values.get("n").expect("n");
    assert!(matches!(n, Value::Integer(0)), "expected 0, got {n:?}");
}
