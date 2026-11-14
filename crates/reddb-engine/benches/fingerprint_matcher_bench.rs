//! Deterministic minutiae matching and exact 1:N benchmarks.
//!
//! Run with:
//! `cargo bench -p reddb-io-engine --bench fingerprint_matcher_bench`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_engine::biometric_matcher::{fingerprint_compare, fingerprint_descriptor_bands};
use reddb_types::{BiometricFormat, BiometricSample};

fn fingerprint(minutiae: usize, x_offset: u16, angle_offset: u8) -> BiometricSample {
    let length = 24 + 4 + minutiae * 6 + 2;
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(b"FMR\0 20\0");
    bytes.extend_from_slice(&(length as u32).to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&[1, 0, 100, minutiae as u8]);
    for index in 0..minutiae {
        let x = 100_u16 + x_offset + (index as u16 % 16) * 40;
        let y = 120_u16 + (index as u16 / 16) * 40;
        let kind = (((index % 2) + 1) as u16) << 14;
        bytes.extend_from_slice(&(x | kind).to_be_bytes());
        bytes.extend_from_slice(&y.to_be_bytes());
        bytes.extend_from_slice(&[((index * 7 + usize::from(angle_offset)) % 256) as u8, 90]);
    }
    bytes.extend_from_slice(&[0, 0]);
    BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, bytes)
        .expect("valid benchmark fingerprint")
}

fn bench_fingerprint_1_to_1(c: &mut Criterion) {
    let left = fingerprint(32, 0, 0);
    let right = fingerprint(32, 10, 1);
    c.bench_function("fingerprint_compare_1_to_1/32_minutiae", |b| {
        b.iter(|| {
            fingerprint_compare(black_box(&left), black_box(&right))
                .expect("compatible fingerprints")
        })
    });
}

fn bench_descriptor_build(c: &mut Criterion) {
    let sample = fingerprint(32, 0, 0);
    c.bench_function("fingerprint_descriptor_build/32_minutiae", |b| {
        b.iter(|| {
            fingerprint_descriptor_bands(black_box(&sample)).expect("descriptor construction")
        })
    });
}

fn bench_exact_1_to_n(c: &mut Criterion) {
    let query = fingerprint(32, 5, 1);
    let corpus = (0..128_u16)
        .map(|index| fingerprint(32, index % 31, (index % 3) as u8))
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("fingerprint_exact_1_to_n");
    group.throughput(Throughput::Elements(corpus.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("templates", corpus.len()),
        &corpus,
        |b, values| {
            b.iter(|| {
                values
                    .iter()
                    .map(|value| {
                        fingerprint_compare(black_box(&query), black_box(value))
                            .expect("compatible fingerprints")
                    })
                    .fold(0.0_f64, f64::max)
            })
        },
    );
    group.finish();
}

criterion_group!(
    benches,
    bench_fingerprint_1_to_1,
    bench_descriptor_build,
    bench_exact_1_to_n
);
criterion_main!(benches);
