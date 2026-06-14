//! Issue #595 — slice 9c of #575.
//!
//! REFRESH MATERIALIZED VIEW now rewrites the backing collection
//! atomically: a concurrent SELECT sees either the prior or the new
//! contents, never partial; a crash mid-refresh (no WAL commit)
//! leaves the prior contents intact on recovery; and
//! `red.materialized_views.current_row_count` reflects the live
//! backing-collection count (mirroring the slice-10 invariant on
//! `queue_pending_gauge` in #527).

#[allow(dead_code)]
mod support;

use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
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
        .expect("spawn materialized view atomic refresh test")
        .join()
        .expect("materialized view atomic refresh test panicked");
}

#[test]
fn refresh_writes_through_backing_collection_and_scrapes_live_count() {
    run_with_large_stack(
        "refresh-writes-through-backing-collection-and-scrapes-live-count",
        refresh_writes_through_backing_collection_and_scrapes_live_count_impl,
    );
}

fn refresh_writes_through_backing_collection_and_scrapes_live_count_impl() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    exec(&rt, "CREATE TABLE orders (id INT, total INT, status TEXT)");
    exec(
        &rt,
        "INSERT INTO orders (id, total, status) VALUES \
           (1, 100, 'paid'), \
           (2, 200, 'paid'), \
           (3, 300, 'pending')",
    );
    exec(
        &rt,
        "CREATE MATERIALIZED VIEW paid_orders AS \
         SELECT id, total FROM orders WHERE status = 'paid'",
    );

    // Before REFRESH: backing collection is empty.
    let pre = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(pre.result.records.len(), 0);
    let pre_count = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "paid_orders")
        .expect("metadata before refresh")
        .current_row_count;
    assert_eq!(pre_count, 0, "live scrape: empty backing = 0 rows");

    // REFRESH atomically swaps in the 2 paid orders.
    exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
    let post = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(post.result.records.len(), 2);
    let post_count = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "paid_orders")
        .expect("metadata after refresh")
        .current_row_count;
    assert_eq!(
        post_count, 2,
        "live scrape reflects backing-collection count"
    );

    // A subsequent REFRESH that yields a different row count also
    // shows up live (no cache-slot lag).
    exec(
        &rt,
        "INSERT INTO orders (id, total, status) VALUES (4, 400, 'paid')",
    );
    exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
    let next = exec(&rt, "SELECT * FROM paid_orders");
    assert_eq!(next.result.records.len(), 3);
    let next_count = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "paid_orders")
        .expect("metadata after 2nd refresh")
        .current_row_count;
    assert_eq!(next_count, 3);
}

#[test]
fn concurrent_reader_sees_old_or_new_never_partial() {
    run_with_large_stack(
        "concurrent-reader-sees-old-or-new-never-partial",
        concurrent_reader_sees_old_or_new_never_partial_impl,
    );
}

fn concurrent_reader_sees_old_or_new_never_partial_impl() {
    // Pre-populate enough source rows that REFRESH is large enough to
    // span at least a few reader iterations. The contract under test
    // is "no intermediate state" — assertion is row count ∈ {N, M},
    // never anything else.
    let rt = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt"));
    exec(&rt, "CREATE TABLE src (id INT, flag INT)");
    let mut values = String::new();
    let initial: i32 = 250;
    for i in 0..initial {
        if i > 0 {
            values.push_str(", ");
        }
        values.push_str(&format!("({i}, 1)"));
    }
    exec(&rt, &format!("INSERT INTO src (id, flag) VALUES {values}"));
    exec(
        &rt,
        "CREATE MATERIALIZED VIEW mv AS SELECT id FROM src WHERE flag = 1",
    );
    exec(&rt, "REFRESH MATERIALIZED VIEW mv");
    let after_first = exec(&rt, "SELECT * FROM mv");
    assert_eq!(after_first.result.records.len(), initial as usize);

    // Insert N more rows so the second REFRESH grows the set; the
    // concurrent reader must observe either `initial` or `initial+grow`,
    // never anything in between (no partial swap).
    let grow: i32 = 250;
    let mut values = String::new();
    for i in initial..(initial + grow) {
        if i > initial {
            values.push_str(", ");
        }
        values.push_str(&format!("({i}, 1)"));
    }
    exec(&rt, &format!("INSERT INTO src (id, flag) VALUES {values}"));

    let stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let rt = Arc::clone(&rt);
        let stop = Arc::clone(&stop);
        readers.push(std::thread::spawn(move || {
            let mut observations = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let r = rt.execute_query("SELECT * FROM mv").expect("select");
                observations.push(r.result.records.len());
            }
            observations
        }));
    }

    // Give readers a small head-start so they get into a tight loop
    // before REFRESH fires, maximising the chance of overlapping.
    std::thread::sleep(Duration::from_millis(20));
    exec(&rt, "REFRESH MATERIALIZED VIEW mv");
    // Let readers continue briefly after the swap to capture post-swap
    // observations.
    std::thread::sleep(Duration::from_millis(20));
    stop.store(true, Ordering::Relaxed);

    let allowed: [usize; 2] = [initial as usize, (initial + grow) as usize];
    let mut total_observations = 0usize;
    let mut saw_old = false;
    let mut saw_new = false;
    for reader in readers {
        let obs = reader.join().expect("reader joined");
        for v in &obs {
            assert!(
                allowed.contains(v),
                "reader observed partial state {v}; allowed = {allowed:?}"
            );
            if *v == allowed[0] {
                saw_old = true;
            }
            if *v == allowed[1] {
                saw_new = true;
            }
            total_observations += 1;
        }
    }
    assert!(total_observations > 0, "readers produced no observations");
    // We can't make a hard guarantee that BOTH states were seen
    // (scheduling). Mostly we want "saw at least new at the end".
    assert!(saw_new, "no reader observed the post-refresh state");
    let _ = saw_old;
}

#[test]
fn refresh_survives_clean_restart() {
    run_with_large_stack(
        "refresh-survives-clean-restart",
        refresh_survives_clean_restart_impl,
    );
}

fn refresh_survives_clean_restart_impl() {
    let path = persistent_path("mv_atomic_refresh_clean");

    // First boot: source → MV → REFRESH → 2 rows visible through MV.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("open");
        exec(&rt, "CREATE TABLE orders (id INT, status TEXT)");
        exec(
            &rt,
            "INSERT INTO orders (id, status) VALUES \
               (1, 'paid'), (2, 'paid'), (3, 'pending')",
        );
        exec(
            &rt,
            "CREATE MATERIALIZED VIEW paid_orders AS \
             SELECT id FROM orders WHERE status = 'paid'",
        );
        exec(&rt, "REFRESH MATERIALIZED VIEW paid_orders");
        let r = exec(&rt, "SELECT * FROM paid_orders");
        assert_eq!(r.result.records.len(), 2);
        rt.checkpoint().expect("checkpoint before restart");
    }

    // Second boot: REFRESH result is durable — backing collection
    // still holds the 2 rows after restart.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path())).expect("reopen");
        let r = exec(&rt, "SELECT * FROM paid_orders");
        assert_eq!(
            r.result.records.len(),
            2,
            "REFRESH must be durable across restart",
        );
        let count = rt
            .materialized_view_metadata()
            .into_iter()
            .find(|m| m.name == "paid_orders")
            .expect("metadata after reopen")
            .current_row_count;
        assert_eq!(count, 2, "scraped count survives reopen");
    }
}

#[test]
fn refresh_failure_leaves_prior_backing_intact() {
    run_with_large_stack(
        "refresh-failure-leaves-prior-backing-intact",
        refresh_failure_leaves_prior_backing_intact_impl,
    );
}

fn refresh_failure_leaves_prior_backing_intact_impl() {
    // Models "crash mid-refresh" by way of an error that aborts the
    // refresh handler before it runs through the atomic swap. The
    // contract is the same: prior backing contents must remain
    // observable — a partial replace is forbidden.
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    exec(&rt, "CREATE TABLE src (id INT)");
    exec(&rt, "INSERT INTO src (id) VALUES (1), (2), (3)");
    exec(&rt, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM src");
    exec(&rt, "REFRESH MATERIALIZED VIEW mv");
    let before = exec(&rt, "SELECT * FROM mv");
    assert_eq!(before.result.records.len(), 3);

    // Drop the source — the next refresh body errors out before any
    // refresh_collection call lands.
    exec(&rt, "DROP TABLE src");
    let result = rt.execute_query("REFRESH MATERIALIZED VIEW mv");
    assert!(result.is_err(), "refresh against missing source must error");

    // Prior backing contents survive.
    let after = exec(&rt, "SELECT * FROM mv");
    assert_eq!(
        after.result.records.len(),
        3,
        "failed refresh must not partially mutate the backing collection"
    );
    let count = rt
        .materialized_view_metadata()
        .into_iter()
        .find(|m| m.name == "mv")
        .expect("metadata after failure")
        .current_row_count;
    assert_eq!(count, 3, "live scrape still reflects prior 3 rows");
}

#[test]
fn refresh_completes_within_reasonable_time_smoke() {
    run_with_large_stack(
        "refresh-completes-within-reasonable-time-smoke",
        refresh_completes_within_reasonable_time_smoke_impl,
    );
}

fn refresh_completes_within_reasonable_time_smoke_impl() {
    // Lightweight perf-regression guard: the atomic-refresh path is
    // O(rows) in the body's row count and a single REFRESH on a few
    // hundred rows should finish well under a second on a dev laptop.
    // The point of this test is to catch accidental O(n²) /
    // unbounded-loop regressions in `refresh_collection` — it is NOT
    // a strict perf SLO. Loose budget on purpose.
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    exec(&rt, "CREATE TABLE src (id INT)");
    let mut values = String::new();
    for i in 0..500 {
        if i > 0 {
            values.push_str(", ");
        }
        values.push_str(&format!("({i})"));
    }
    exec(&rt, &format!("INSERT INTO src (id) VALUES {values}"));
    exec(&rt, "CREATE MATERIALIZED VIEW mv AS SELECT id FROM src");

    let started = Instant::now();
    exec(&rt, "REFRESH MATERIALIZED VIEW mv");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "REFRESH on 500 rows took {elapsed:?}; expected well under 5s"
    );
    let r = exec(&rt, "SELECT * FROM mv");
    assert_eq!(r.result.records.len(), 500);
}
