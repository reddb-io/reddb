//! TurboQuant scalar-vs-dispatched scoring benchmark.
//!
//! Run on ARM with:
//!   cargo bench -p reddb-io-server --bench turboquant_neon_bench

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use reddb_server::storage::engine::turboquant::codec::EncodedVector;
use reddb_server::storage::engine::turboquant::scoring::{
    detect_score_kernel, score_units, score_units_scalar, QueryLut,
};
use std::hint::black_box;

const VECTOR_COUNT: usize = 100_000;
const DIM: usize = 1_536;
const BYTE_GROUPS: usize = DIM / 2;

fn synthetic_query() -> Vec<f32> {
    (0..DIM)
        .map(|i| {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            sign * ((i % 31) as f32 / 31.0)
        })
        .collect()
}

fn synthetic_centroids() -> Vec<f64> {
    (0..16)
        .map(|code| -1.0 + (code as f64 + 0.5) * 0.125)
        .collect()
}

fn synthetic_encoded() -> Vec<EncodedVector> {
    (0..VECTOR_COUNT)
        .map(|row| {
            let packed = (0..BYTE_GROUPS)
                .map(|group| {
                    let lo = ((row + group) & 0x0f) as u8;
                    let hi = ((row * 3 + group * 5) & 0x0f) as u8;
                    lo | (hi << 4)
                })
                .collect();
            EncodedVector { packed, scale: 1.0 }
        })
        .collect()
}

fn turboquant_scoring_d1536_100k(c: &mut Criterion) {
    let query = synthetic_query();
    let lut = QueryLut::build(&query, &synthetic_centroids());
    let encoded = synthetic_encoded();
    eprintln!(
        "[turboquant_neon_bench] detected_kernel={:?} vectors={} dim={}",
        detect_score_kernel(),
        VECTOR_COUNT,
        DIM
    );

    let mut group = c.benchmark_group("turboquant-scoring-d1536-100k");
    group.throughput(Throughput::Elements(VECTOR_COUNT as u64));
    group.sample_size(10);

    group.bench_function("scalar-fallback", |b| {
        b.iter(|| black_box(score_units_scalar(black_box(&lut), black_box(&encoded))))
    });

    group.bench_function("runtime-dispatch", |b| {
        b.iter(|| black_box(score_units(black_box(&lut), black_box(&encoded))))
    });

    group.finish();
}

criterion_group!(benches, turboquant_scoring_d1536_100k);
criterion_main!(benches);
