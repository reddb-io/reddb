//! Issue #859 — Phase 1 (verify): sealed *columnar* chunks evict via the
//! EXISTING hypertable TTL / `drop_chunks` / partition-prune path.
//!
//! PRD #850 decision: reuse the live hypertable time-partition + TTL — do
//! not rebuild it. A columnar chunk is one whose `ChunkMeta.columnar_page`
//! is `Some(..)` (the RDCC `ColumnBlock` discriminant). These end-to-end
//! tests inject such a chunk through the public registry (the same
//! `restore_chunk` call the boot path uses) and drive eviction + pruning
//! over the SQL scalar surface, proving the retention path is storage-form
//! agnostic: columnar chunks drop in O(1) metadata work (no per-row
//! delete) and are pruned out of time-range queries exactly like row
//! chunks. No new TTL/partition subsystem is involved.

use reddb::application::ExecuteQueryInput;
use reddb::storage::engine::PageLocation;
use reddb::storage::schema::Value;
use reddb::storage::timeseries::{ChunkId, ChunkMeta};
use reddb::{QueryUseCases, RedDBRuntime};

const HOUR_NS: u64 = 3_600_000_000_000;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

/// A sealed columnar chunk: carries an RDCC `columnar_page` and one row at
/// `max_ts_ns` so it has a finite TTL expiry.
fn columnar_chunk(hypertable: &str, start_ns: u64, max_ts_ns: u64) -> ChunkMeta {
    let mut meta = ChunkMeta::new(
        ChunkId {
            hypertable: hypertable.into(),
            start_ns,
        },
        start_ns + HOUR_NS,
    );
    meta.row_count = 1;
    meta.min_ts_ns = max_ts_ns;
    meta.max_ts_ns = max_ts_ns;
    meta.sealed = true;
    meta.columnar_page = Some(PageLocation::new(7, 0, 1234));
    meta
}

#[test]
fn columnar_chunk_evicts_via_sweep_expired_over_sql() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    // 1-hour chunks, 1-hour TTL — a chunk with max_ts=0 expires at 1h.
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1h'".into(),
    })
    .expect("create ok");

    // Inject a sealed columnar chunk — the read-bridge / INSERT→seal wiring
    // is out of scope (#861); we restore the chunk the boot path would.
    rt.db()
        .hypertables()
        .restore_chunk(columnar_chunk("metrics", 0, 0));
    assert!(
        rt.db().hypertables().show_chunks("metrics")[0]
            .columnar_page
            .is_some(),
        "precondition: the chunk is columnar-backed"
    );

    // Sweep at 3h via the existing scalar — the columnar chunk must drop.
    let now_ns = 3 * HOUR_NS;
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT HYPERTABLE_SWEEP_EXPIRED('metrics', {now_ns}) AS n"),
        })
        .expect("sweep ok");
    let n = r.result.records[0].get("n").expect("n");
    assert!(
        matches!(n, Value::Integer(1)),
        "columnar chunk must evict via the existing TTL sweep, got {n:?}"
    );
    assert!(
        rt.db().hypertables().show_chunks("metrics").is_empty(),
        "sweep must reclaim the columnar chunk metadata"
    );
}

#[test]
fn columnar_chunk_drops_via_drop_chunks_before_over_sql() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    rt.db()
        .hypertables()
        .restore_chunk(columnar_chunk("metrics", 0, 0));

    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT HYPERTABLE_DROP_CHUNKS_BEFORE('metrics', {HOUR_NS}) AS n"),
        })
        .expect("drop ok");
    let n = r.result.records[0].get("n").expect("n");
    assert!(
        matches!(n, Value::Integer(1)),
        "drop_chunks_before must drop the columnar chunk, got {n:?}"
    );
    assert!(rt.db().hypertables().show_chunks("metrics").is_empty());
}

#[test]
fn columnar_chunk_is_pruned_outside_time_range_over_sql() {
    // Acceptance #2: partition pruning (Phase 0 #902) holds for columnar
    // chunks — the pruner selects on chunk bounds, never on columnar_page.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("create ok");
    let db = rt.db();
    let reg = db.hypertables();
    reg.restore_chunk(columnar_chunk("metrics", 0, 0));
    reg.restore_chunk(columnar_chunk("metrics", HOUR_NS, HOUR_NS));
    reg.restore_chunk(columnar_chunk("metrics", 2 * HOUR_NS, 2 * HOUR_NS));

    // Window [1h, 2h) overlaps exactly the middle columnar chunk.
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
    match kept {
        Value::Array(v) => assert_eq!(
            v.len(),
            1,
            "exactly one in-window columnar chunk kept, got {v:?}"
        ),
        other => panic!("expected Array, got {other:?}"),
    }
}
