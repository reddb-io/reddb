//! Issue #815 — slice of PRD #811 (replication convergence).
//!
//! A large WAL re-apply on a replica must not wedge the HTTP surface.
//! The observed failure was a ~46h freeze: the replica container went
//! `unhealthy` and `/catalog` timed out while a big re-sync ran, because
//! the catalog snapshot held the store's `collections` read lock across a
//! full-store scan. Under `parking_lot`, a writer that parks behind that
//! long read (the apply loop creating a new collection) in turn parks
//! every subsequent reader — so `/catalog` and readiness wedged behind a
//! single slow scan colliding with apply writes.
//!
//! ## What this test pins
//!
//! The replica apply loop already releases the store lock *per record*
//! (it applies one `ChangeRecord` at a time and never holds a lock across
//! the batch). The remaining responsiveness risk was on the read side:
//! `UnifiedStore::query_all` (the `/catalog` scan) and `UnifiedStore::stats`
//! (the readiness/`health()` scan) held `collections.read()` across the
//! whole scan. Both now snapshot the per-collection manager handles under a
//! brief read lock and scan off the map lock.
//!
//! This test drives the *same* `UnifiedStore` the replica applier mutates:
//! a writer thread continuously creates new collections (collections-map
//! write lock — exactly what `get_or_create_collection` / `insert_auto`
//! take during apply) and inserts rows, simulating a large re-apply. While
//! that runs, the "HTTP surface" thread repeatedly takes a catalog snapshot
//! and a health/readiness report and asserts each call returns well within
//! a timeout. A store-lock wedge would blow the per-call budget; the
//! decoupled scans keep it fast.

#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::{RedDBOptions, RedDBRuntime};

fn temp_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn exec(query: &QueryUseCases<'_, RedDBRuntime>, sql: &str) {
    query
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"));
}

#[test]
fn catalog_and_readiness_stay_responsive_during_large_reapply() {
    let path = temp_path("responsive");

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("open store");

    // Seed several collections with rows so a catalog/health scan does
    // real work — a no-op snapshot could pass the budget trivially.
    {
        let query = QueryUseCases::new(&rt);
        for c in 0..8 {
            exec(&query, &format!("CREATE TABLE seed_{c} (id INT, val TEXT)"));
            for r in 0..200 {
                exec(
                    &query,
                    &format!("INSERT INTO seed_{c} (id, val) VALUES ({r}, 'v{r}')"),
                );
            }
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicU64::new(0));

    // Writer thread — stands in for the replica apply loop under a large
    // re-apply: continuously create new collections (collections-map write
    // lock) and insert rows into the shared store.
    let writer = {
        let rt = rt.clone();
        let stop = stop.clone();
        let writes = writes.clone();
        thread::spawn(move || {
            let query = QueryUseCases::new(&rt);
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = query.execute(ExecuteQueryInput {
                    query: format!("CREATE TABLE reapply_{n} (id INT, val TEXT)"),
                });
                for r in 0..50 {
                    let _ = query.execute(ExecuteQueryInput {
                        query: format!("INSERT INTO reapply_{n} (id, val) VALUES ({r}, 'x{r}')"),
                    });
                }
                n += 1;
                writes.store(n, Ordering::Relaxed);
            }
        })
    };

    // "HTTP surface" thread (this thread): repeatedly snapshot the catalog
    // and pull a health/readiness report, asserting each call stays well
    // within budget. The real wedge took hours; 5s is a generous ceiling
    // that still catches a genuine store-lock freeze.
    let db = rt.db();
    let per_call_budget = Duration::from_secs(5);
    let mut max_latency = Duration::ZERO;
    let mut iterations = 0u64;
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let started = Instant::now();
        let snapshot = reddb::catalog::snapshot_store("responsive", db.store().as_ref(), None);
        // readiness/health walks `stats()` — the other formerly lock-held scan.
        let _health = db.health();
        let elapsed = started.elapsed();
        max_latency = max_latency.max(elapsed);

        assert!(
            !snapshot.collections.is_empty(),
            "catalog snapshot should observe the seeded collections"
        );
        assert!(
            elapsed < per_call_budget,
            "catalog/readiness call wedged under apply load: took {elapsed:?} \
             (budget {per_call_budget:?}, max so far {max_latency:?})"
        );
        iterations += 1;
    }

    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer thread should join");

    assert!(
        iterations >= 5,
        "expected several responsive catalog/readiness reads under load, got {iterations}"
    );
    assert!(
        writes.load(Ordering::Relaxed) >= 1,
        "writer should have made apply progress while reads ran — \
         confirms reads and writes interleaved rather than serialising"
    );

    drop(rt);
}
