//! Result-cache read-path benchmark.
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench result_cache_bench
//!
//! Each workload compares the authoritative executor with a verified hot
//! result-cache hit. Runtime construction and data loading are outside the
//! measured loop.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use reddb_server::RedDBRuntime;
use std::hint::black_box;

const SCALAR_QUERY: &str = "SELECT 1 AS n";
const POINT_QUERY: &str = "SELECT n FROM result_cache_bench WHERE id = 1";
const AGGREGATE_QUERY: &str = "SELECT SUM(n) AS n FROM result_cache_bench";
const ROWS: usize = 2_048;

fn exec(runtime: &RedDBRuntime, sql: &str) {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"));
}

fn runtime(cache_enabled: bool, query: &str) -> RedDBRuntime {
    let runtime = RedDBRuntime::in_memory().expect("in-memory runtime");
    exec(&runtime, "SET CONFIG runtime.result_cache.enabled = false");
    exec(
        &runtime,
        "CREATE TABLE result_cache_bench (id INT PRIMARY KEY, n INT)",
    );
    for first in (1..=ROWS).step_by(256) {
        let last = (first + 255).min(ROWS);
        let values = (first..=last)
            .map(|id| format!("({id}, {id})"))
            .collect::<Vec<_>>()
            .join(",");
        exec(
            &runtime,
            &format!("INSERT INTO result_cache_bench (id, n) VALUES {values}"),
        );
    }
    exec(
        &runtime,
        &format!("SET CONFIG runtime.result_cache.enabled = {cache_enabled}"),
    );
    if cache_enabled {
        exec(&runtime, query);
        let before = runtime.result_cache_metrics();
        exec(&runtime, query);
        assert_eq!(
            runtime.result_cache_metrics().0 - before.0,
            1,
            "benchmark setup did not produce a hot cache hit"
        );
    }
    runtime
}

fn bench_query(c: &mut Criterion, group_name: &str, query: &'static str) {
    let disabled = runtime(false, query);
    let enabled = runtime(true, query);
    let mut group = c.benchmark_group(group_name);
    for (mode, runtime) in [("cache_off", &disabled), ("hot_hit", &enabled)] {
        group.bench_with_input(BenchmarkId::from_parameter(mode), runtime, |b, runtime| {
            b.iter(|| {
                let result = runtime
                    .execute_query(black_box(query))
                    .expect("benchmark query");
                black_box(result.result.records.len())
            });
        });
    }
    group.finish();
}

fn result_cache_bench(c: &mut Criterion) {
    bench_query(c, "result_cache/scalar", SCALAR_QUERY);
    bench_query(c, "result_cache/indexed_point_read", POINT_QUERY);
    bench_query(c, "result_cache/aggregate_2048_rows", AGGREGATE_QUERY);
}

criterion_group!(benches, result_cache_bench);
criterion_main!(benches);
