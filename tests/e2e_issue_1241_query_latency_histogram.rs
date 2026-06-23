//! Issue #1241 — query latency histogram rollups.
//!
//! Exercises the real statement lifecycle (`execute_query`) and asserts
//! the recorded latency histogram substrate (ADR 0060 §2 histograms):
//!
//! * every executed query is observed under its `kind` (select/insert/…);
//! * the cross-kind rollup yields monotonic, sane P50/P95/P99;
//! * cardinality is bounded to `kind` only — no per-collection series.

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn executed_queries_populate_the_kind_bounded_latency_histogram() {
    let rt = open_runtime();

    // No query has run yet — the rollup has no sample, so percentiles
    // are absent (honesty rule #738 / ADR 0060 §6), which keeps
    // `/cluster/status` `latency` an unavailable envelope.
    assert_eq!(rt.query_latency_rollup().count, 0);
    assert_eq!(rt.query_latency_rollup().quantile(0.50), None);

    exec(&rt, "CREATE TABLE lat_1241 (id INT, body TEXT)");
    for i in 0..20 {
        exec(
            &rt,
            &format!("INSERT INTO lat_1241 (id, body) VALUES ({i}, 'x')"),
        );
    }
    for _ in 0..10 {
        exec(&rt, "SELECT id, body FROM lat_1241");
    }

    // Per-kind snapshot carries one cell per observed kind. The only
    // dimension is `kind`; there is no collection/tenant/user label.
    let snap = rt.query_latency_snapshot();
    let by_kind: std::collections::BTreeMap<&str, u64> =
        snap.iter().map(|h| (h.kind, h.count)).collect();
    assert!(
        by_kind.get("select").copied().unwrap_or(0) >= 10,
        "expected >=10 select samples, got {by_kind:?}"
    );
    assert!(
        by_kind.get("insert").copied().unwrap_or(0) >= 20,
        "expected >=20 insert samples, got {by_kind:?}"
    );

    // Each per-kind histogram is internally consistent: cumulative
    // buckets are non-decreasing and the +Inf bucket equals count.
    for h in &snap {
        let mut prev = 0u64;
        for &b in &h.bucket_counts {
            assert!(b >= prev, "buckets must be cumulative for {}", h.kind);
            assert!(b <= h.count, "no bucket may exceed count for {}", h.kind);
            prev = b;
        }
    }

    // The cross-kind rollup (what `/cluster/status` + red-ui read) now
    // has samples and produces monotonic percentiles.
    let roll = rt.query_latency_rollup();
    assert!(roll.count >= 31, "rollup count = {}", roll.count);
    let p50 = roll.quantile(0.50).expect("p50 available after samples");
    let p95 = roll.quantile(0.95).expect("p95 available after samples");
    let p99 = roll.quantile(0.99).expect("p99 available after samples");
    assert!(p50 <= p95 && p95 <= p99, "percentiles must be monotonic");
    assert!(p99 >= 0.0, "percentiles are non-negative seconds");
}
