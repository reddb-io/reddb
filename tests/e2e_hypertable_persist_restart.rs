//! Issue #866 — Phase 0: the hypertable chunk spine must survive a
//! restart. Create a hypertable, allocate chunks across several time
//! buckets, checkpoint, reopen the engine, and confirm chunk bounds /
//! routing / TTL come back identical — and that a write after the
//! restart routes to the correct existing chunk instead of allocating
//! a duplicate.
//!
//! Persistence rides the SAME metadata path as collection contracts
//! (the physical metadata sidecar), written by `persist_metadata()` on
//! the checkpoint/flush durability boundary — not a parallel durability
//! mechanism.

mod support;

use support::{checkpoint_and_reopen, PersistentDbPath};

use reddb::application::ExecuteQueryInput;
use reddb::storage::timeseries::ChunkMeta;
use reddb::QueryUseCases;

const DAY_NS: u64 = 86_400_000_000_000;
const HOUR_NS: u64 = 3_600_000_000_000;

/// Stable comparison key for a chunk — every field that must round-trip.
fn chunk_fingerprint(m: &ChunkMeta) -> (u64, u64, u64, u64, u64, bool, Option<u64>) {
    (
        m.id.start_ns,
        m.end_ns_exclusive,
        m.row_count,
        m.min_ts_ns,
        m.max_ts_ns,
        m.sealed,
        m.ttl_override_ns,
    )
}

#[test]
fn hypertable_chunk_metadata_survives_restart() {
    let path = PersistentDbPath::new("hypertable_persist_restart");
    let rt = path.open_runtime();
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '7d'".into(),
    })
    .expect("create hypertable ok");

    // Write across four distinct day-chunks. Multiple writes land in
    // the first chunk so row_count / min / max are non-trivial.
    {
        let db = rt.db();
        let reg = db.hypertables();
        for t in [10u64, 100, DAY_NS - 1] {
            reg.route("metrics", t).expect("route day-0");
        }
        reg.route("metrics", DAY_NS + 5).expect("route day-1");
        reg.route("metrics", 2 * DAY_NS + 9).expect("route day-2");
        let last = reg.route("metrics", 5 * DAY_NS).expect("route day-5");

        // Exercise the bits that are NOT derivable from row data: a
        // sealed flag and a per-chunk TTL override must persist too.
        assert!(reg.seal_chunk(&last));
        assert!(reg.set_chunk_ttl_ns(&last, Some(3 * HOUR_NS)));
    }

    // Snapshot pre-restart state.
    let before: Vec<_> = rt
        .db()
        .hypertables()
        .show_chunks("metrics")
        .iter()
        .map(chunk_fingerprint)
        .collect();
    assert_eq!(before.len(), 4, "four chunks allocated pre-restart");

    // Durable checkpoint, then reopen the engine from disk.
    let rt = checkpoint_and_reopen(&path, rt);

    // Spec recovered identically.
    let spec = rt
        .db()
        .hypertables()
        .get("metrics")
        .expect("hypertable spec recovered after restart");
    assert_eq!(spec.time_column, "ts");
    assert_eq!(spec.chunk_interval_ns, DAY_NS);
    assert_eq!(
        spec.default_ttl_ns,
        Some(7 * DAY_NS),
        "default TTL recovered after restart"
    );

    // Chunks recovered identically — bounds, counts, sealed, override.
    let after: Vec<_> = rt
        .db()
        .hypertables()
        .show_chunks("metrics")
        .iter()
        .map(chunk_fingerprint)
        .collect();
    assert_eq!(
        after, before,
        "chunk metadata must be recovered identical to pre-restart"
    );

    // The sealed + TTL-overridden chunk specifically survived.
    let recovered = rt.db().hypertables().show_chunks("metrics");
    let last = recovered
        .iter()
        .find(|c| c.id.start_ns == 5 * DAY_NS)
        .expect("day-5 chunk recovered");
    assert!(last.sealed, "sealed flag must survive restart");
    assert_eq!(last.ttl_override_ns, Some(3 * HOUR_NS));

    // A write after restart routes to the EXISTING day-0 chunk — no
    // duplicate / incorrect allocation.
    let db = rt.db();
    let reg = db.hypertables();
    let routed = reg.route("metrics", 42).expect("route after restart");
    assert_eq!(routed.start_ns, 0, "post-restart write routes to day-0");
    assert_eq!(
        reg.show_chunks("metrics").len(),
        4,
        "post-restart write must not allocate a new chunk"
    );
    // The existing chunk's row_count advanced from the recovered value.
    let day0 = reg
        .show_chunks("metrics")
        .into_iter()
        .find(|c| c.id.start_ns == 0)
        .expect("day-0 chunk");
    assert_eq!(
        day0.row_count, 4,
        "3 pre-restart rows + 1 post-restart row in day-0 chunk"
    );
}

#[test]
fn non_hypertable_database_persists_no_hypertables() {
    // Guard: a database that never declared a hypertable carries an
    // empty hypertable spine — the persist step is a no-op for it.
    let path = PersistentDbPath::new("no_hypertable_persist");
    let rt = path.open_runtime();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE plain (id INTEGER, name TEXT)".into(),
    })
    .expect("create table ok");

    let rt = checkpoint_and_reopen(&path, rt);
    assert!(
        rt.db().hypertables().is_empty(),
        "no hypertables should be registered after restart"
    );
}
