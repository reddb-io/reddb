//! Issue #592 — Analytics slice 7d: 1M-row window perf smoke.
//!
//! Acceptance: "Performance smoke: window on 1M rows with
//! single-column PARTITION BY completes within an order of
//! magnitude of the equivalent Postgres timing (no algorithmic
//! regression like full sort per partition)."
//!
//! PG-17 on commodity hardware computes
//!   `SUM(amount) OVER (PARTITION BY user_id ORDER BY ts)`
//! on 1M rows with a single partition in roughly 1-3 seconds.
//! "Within an order of magnitude" lets us assert a generous
//! 60-second ceiling here: the test catches an algorithmic
//! regression like an accidental O(N^2) inner loop or a per-row
//! re-sort, but tolerates host noise.
//!
//! `#[ignore]`-gated because perf measurement does not belong in
//! the default cadence (noisy, slow). Run explicitly with:
//!   cargo test --release --test window_perf_smoke -- --ignored --nocapture

use std::time::{Duration, Instant};

use reddb::application::ExecuteQueryInput;
use reddb::{QueryUseCases, RedDBRuntime};

const N_ROWS: usize = 1_000_000;
const WINDOW_QUERY_BUDGET_SECS: u64 = 60;

#[test]
#[ignore = "perf smoke — run explicitly with --ignored --nocapture"]
fn window_running_total_1m_rows_single_partition_completes_within_budget() {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE purchases (id INTEGER, user_id TEXT, ts BIGINT, amount BIGINT)"
            .into(),
    })
    .expect("create table");

    // Ingestion phase. Not part of the perf budget (insert performance
    // is its own concern, tracked elsewhere). We use multi-row VALUES
    // batches to keep ingestion fast enough to leave time for the
    // window measurement on a CI runner.
    let ingest_start = Instant::now();
    const BATCH: usize = 500;
    let mut id: i64 = 0;
    while (id as usize) < N_ROWS {
        let mut sql = String::with_capacity(BATCH * 64);
        sql.push_str("INSERT INTO purchases (id, user_id, ts, amount) VALUES ");
        let batch_end = ((id as usize) + BATCH).min(N_ROWS);
        for i in (id as usize)..batch_end {
            if i > id as usize {
                sql.push(',');
            }
            // Single partition (every row in 'u1') so the window
            // phase has a 1M-row partition to scan/sort once.
            sql.push_str(&format!("({}, 'u1', {}, {})", i + 1, i + 1, (i % 1000) + 1));
        }
        q.execute(ExecuteQueryInput { query: sql }).expect("insert batch");
        id = batch_end as i64;
    }
    let ingest = ingest_start.elapsed();
    eprintln!("ingest 1M rows: {ingest:?}");

    // The acceptance check: time the window query itself.
    let query_start = Instant::now();
    let res = q
        .execute(ExecuteQueryInput {
            query: "SELECT id, SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) AS running \
                    FROM purchases"
                .into(),
        })
        .expect("window query");
    let elapsed = query_start.elapsed();
    eprintln!(
        "window query over 1M rows: {elapsed:?} ({} rows out)",
        res.result.records.len()
    );

    assert_eq!(
        res.result.records.len(),
        N_ROWS,
        "window must emit one row per input row, not collapse",
    );

    let budget = Duration::from_secs(WINDOW_QUERY_BUDGET_SECS);
    assert!(
        elapsed < budget,
        "window over 1M rows took {elapsed:?}, exceeds {budget:?} budget — \
         likely an algorithmic regression (e.g. O(N^2) per-row re-sort)",
    );
}
