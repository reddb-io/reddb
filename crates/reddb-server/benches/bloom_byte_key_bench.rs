//! Byte-key bloom benchmark (#1827).
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench bloom_byte_key_bench
//!
//! Compares the legacy byte-slice `BloomFilter` against the byte-key front end
//! on `SplitBlockBloom` within one Criterion run. Cross-run comparisons are
//! not valid.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use reddb_server::storage::primitives::bloom::BloomFilter;
use reddb_server::storage::primitives::split_block_bloom::SplitBlockBloom;
use std::hint::black_box;

const CAPACITY: usize = 100_000;

fn key(i: u64) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&i.to_le_bytes());
    key[8..].copy_from_slice(&i.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    key
}

fn keys(start: u64) -> Vec<[u8; 16]> {
    (start..start + CAPACITY as u64).map(key).collect()
}

fn legacy_filter(keys: &[[u8; 16]]) -> BloomFilter {
    let mut filter = BloomFilter::with_capacity(CAPACITY, 0.01);
    for key in keys {
        filter.insert(black_box(key));
    }
    filter
}

fn split_filter(keys: &[[u8; 16]]) -> SplitBlockBloom {
    let mut filter = SplitBlockBloom::with_capacity(CAPACITY);
    for key in keys {
        filter.insert_bytes(black_box(key));
    }
    filter
}

fn bench_bloom_byte_keys(c: &mut Criterion) {
    let present = keys(0);
    let absent = keys(CAPACITY as u64);
    let legacy = legacy_filter(&present);
    let split = split_filter(&present);

    let mut group = c.benchmark_group("bloom-byte-key");
    group.sample_size(20);
    group.throughput(Throughput::Elements(CAPACITY as u64));

    group.bench_function("legacy-insert", |b| {
        b.iter_batched(
            || BloomFilter::with_capacity(CAPACITY, 0.01),
            |mut filter| {
                for key in &present {
                    filter.insert(black_box(key));
                }
                black_box(filter)
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("legacy-contains-present", |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for key in &present {
                matches += usize::from(legacy.contains(black_box(key)));
            }
            black_box(matches)
        });
    });

    group.bench_function("legacy-contains-absent", |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for key in &absent {
                matches += usize::from(legacy.contains(black_box(key)));
            }
            black_box(matches)
        });
    });

    group.bench_function("split-insert-bytes", |b| {
        b.iter_batched(
            || SplitBlockBloom::with_capacity(CAPACITY),
            |mut filter| {
                for key in &present {
                    filter.insert_bytes(black_box(key));
                }
                black_box(filter)
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("split-probe-bytes-present", |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for key in &present {
                matches += usize::from(split.probe_bytes(black_box(key)));
            }
            black_box(matches)
        });
    });

    group.bench_function("split-probe-bytes-absent", |b| {
        b.iter(|| {
            let mut matches = 0usize;
            for key in &absent {
                matches += usize::from(split.probe_bytes(black_box(key)));
            }
            black_box(matches)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_bloom_byte_keys);
criterion_main!(benches);
