//! Plain timeseries ingest benchmark (#1810).
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench timeseries_ingest_bench
//!
//! The two scenarios run in the same Criterion group so native
//! timeseries chunk-routing throughput is measured against a local
//! row-table insert baseline in one bench run.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::hint::black_box;

const ROWS: usize = 1_000;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn timeseries_values(rows: usize) -> String {
    (0..rows)
        .map(|i| {
            format!(
                "('cpu.usage', {:.1}, {{host: 'srv{}'}}, {})",
                50.0 + (i % 50) as f64,
                i % 16,
                i as u64 * 1_000_000_000
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn row_values(rows: usize) -> String {
    (0..rows)
        .map(|i| {
            format!(
                "('cpu.usage', {:.1}, 'srv{}', {})",
                50.0 + (i % 50) as f64,
                i % 16,
                i as u64 * 1_000_000_000
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn ingest_timeseries(values: &str) -> u64 {
    let rt = runtime();
    exec(&rt, "CREATE TIMESERIES cpu RETENTION 7 d");
    exec(
        &rt,
        &format!("INSERT INTO cpu (metric, value, tags, timestamp) VALUES {values}"),
    );
    rt.db().hypertables().total_rows("cpu")
}

fn ingest_row_table(values: &str) -> u64 {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE cpu_rows (metric TEXT, value FLOAT, host TEXT, timestamp BIGINT)",
    );
    exec(
        &rt,
        &format!("INSERT INTO cpu_rows (metric, value, host, timestamp) VALUES {values}"),
    );
    rt.db()
        .store()
        .get_collection("cpu_rows")
        .map(|manager| manager.stats().total_entities as u64)
        .unwrap_or(0)
}

fn bench_timeseries_ingest(c: &mut Criterion) {
    let ts_values = timeseries_values(ROWS);
    let table_values = row_values(ROWS);

    let mut group = c.benchmark_group("timeseries-ingest");
    group.sample_size(20);
    group.throughput(Throughput::Elements(ROWS as u64));

    group.bench_function("plain-timeseries-chunked-native-points", |b| {
        b.iter_batched(
            || ts_values.clone(),
            |values| black_box(ingest_timeseries(black_box(&values))),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("row-table-reference", |b| {
        b.iter_batched(
            || table_values.clone(),
            |values| black_box(ingest_row_table(black_box(&values))),
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_timeseries_ingest);
criterion_main!(benches);
