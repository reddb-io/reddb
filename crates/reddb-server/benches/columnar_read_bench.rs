//! Columnar-vs-row read benchmark (#943/#962, PRD #850 Phase 2).
//!
//! Reproducible gate harness measuring both decode paths on the same sealed
//! columnar chunk workload. Baseline recorded by #943; optimised by #962.
//!
//! Run with:
//!   cargo bench -p reddb-io-server --bench columnar_read_bench
//!
//! Results feed docs/perf/2026-06-03-columnar-read.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_server::storage::query::batch::column_batch_from_block;
use reddb_server::storage::timeseries::chunk::{
    points_from_column_block, TimeSeriesChunk, COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID,
};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Seal a synthetic chunk of `n` rows: timestamps 1 ms apart starting at a
/// realistic epoch, values cycling over a small range to reproduce the codec
/// pattern (DoubleDelta+LZ4 for ts, Xor+LZ4 for values after #962).
fn sealed_chunk(n: usize) -> Vec<u8> {
    let mut chunk = TimeSeriesChunk::with_max_points("cpu.idle", Default::default(), n.max(1));
    for i in 0..n {
        assert!(
            chunk.append(
                1_700_000_000_000 + i as u64 * 1_000_000,
                95.0 + (i % 7) as f64 * 0.25,
            ),
            "append failed at row {i}"
        );
    }
    chunk.seal_columnar(7, 1).expect("seal columnar chunk")
}

const CHUNK_SIZES: &[usize] = &[1_000, 10_000, 50_000];
const GUARDED_RATIO_REPS: u32 = 16;

/// Row-at-a-time path: `points_from_column_block` → `Vec<TimeSeriesPoint>`.
fn bench_row_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("columnar-read/row-path");
    group.sample_size(30);
    for &n in CHUNK_SIZES {
        let block = sealed_chunk(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &block, |b, block| {
            b.iter(|| {
                let points = points_from_column_block(black_box(block)).expect("row decode");
                black_box(points)
            });
        });
    }
    group.finish();
}

/// Columnar batch path: `column_batch_from_block` → `ColumnBatch` (both cols).
fn bench_columnar_path(c: &mut Criterion) {
    let projection = [COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID];
    let mut group = c.benchmark_group("columnar-read/batch-path");
    group.sample_size(30);
    for &n in CHUNK_SIZES {
        let block = sealed_chunk(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &block, |b, block| {
            b.iter(|| {
                let batch = column_batch_from_block(black_box(block), black_box(&projection))
                    .expect("batch decode");
                black_box(batch)
            });
        });
    }
    group.finish();
}

/// Projection pushdown: single-column scan (timestamp only).
/// Isolates the codec+copy cost for one column vs both.
fn bench_columnar_ts_only(c: &mut Criterion) {
    let projection = [COLUMNAR_TS_COLUMN_ID];
    let mut group = c.benchmark_group("columnar-read/batch-ts-only");
    group.sample_size(30);
    for &n in CHUNK_SIZES {
        let block = sealed_chunk(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &block, |b, block| {
            b.iter(|| {
                let batch = column_batch_from_block(black_box(block), black_box(&projection))
                    .expect("ts-only batch decode");
                black_box(batch)
            });
        });
    }
    group.finish();
}

/// Guarded lane for the projection read-win claim.
///
/// The row and projection paths are measured in one Criterion run, and the
/// explicit ratio is derived from a paired timing pass over the same blocks.
fn bench_projection_vs_row_guard(c: &mut Criterion) {
    let projection = [COLUMNAR_TS_COLUMN_ID];
    let mut group = c.benchmark_group("columnar-read/projection-vs-row-guard");
    group.sample_size(10);
    for &n in CHUNK_SIZES {
        let block = sealed_chunk(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("row-scan", n), &block, |b, block| {
            b.iter(|| {
                let points = points_from_column_block(black_box(block)).expect("row decode");
                black_box(points)
            });
        });
        group.bench_with_input(
            BenchmarkId::new("columnar-projection-scan", n),
            &block,
            |b, block| {
                b.iter(|| {
                    let batch = column_batch_from_block(black_box(block), black_box(&projection))
                        .expect("ts-only batch decode");
                    black_box(batch)
                });
            },
        );
        let (row, projection) = paired_projection_ratio(&block, &projection);
        let ratio = projection.as_secs_f64() / row.as_secs_f64();
        eprintln!(
            "[columnar-read guard] rows={n} row_scan={row:?} columnar_projection_scan={projection:?} ratio(projection/row)={ratio:.3}"
        );
    }
    group.finish();
}

fn paired_projection_ratio(block: &[u8], projection: &[u32]) -> (Duration, Duration) {
    let mut row_total = Duration::ZERO;
    let mut projection_total = Duration::ZERO;
    for _ in 0..GUARDED_RATIO_REPS {
        let start = Instant::now();
        let points = points_from_column_block(black_box(block)).expect("row decode");
        black_box(points);
        row_total += start.elapsed();

        let start = Instant::now();
        let batch =
            column_batch_from_block(black_box(block), black_box(projection)).expect("batch decode");
        black_box(batch);
        projection_total += start.elapsed();
    }
    (
        row_total / GUARDED_RATIO_REPS,
        projection_total / GUARDED_RATIO_REPS,
    )
}

criterion_group!(
    benches,
    bench_row_path,
    bench_columnar_path,
    bench_columnar_ts_only,
    bench_projection_vs_row_guard
);
criterion_main!(benches);
