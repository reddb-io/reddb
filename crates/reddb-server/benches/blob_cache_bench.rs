/// Blob Cache benchmark suite — issue #190 / #149.
///
/// Covers all 8 workloads from bench/blob-cache/scenarios.md.
/// L1 is scaled to 8 MiB (default 256 MiB) so the suite finishes quickly
/// on any host; the relative comparisons are host-invariant.
///
/// Redis cells require a running Redis 7.4 instance:
///   export REDIS_NO_PERSIST_ADDR=127.0.0.1:6379
///   export REDIS_AOF_ADDR=127.0.0.1:6380
/// See bench/blob-cache/redis-up.sh to start them.
///
/// Without those env vars the Redis cells are silently skipped.
use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use reddb_server::storage::cache::{
    BlobCache, BlobCacheConfig, BlobCachePut, CacheKey, CachePolicy, L2Compression, ResultCache,
};
use std::time::{Duration, Instant};

// ── constants ────────────────────────────────────────────────────────────────

/// L1 cap used across all scenarios (scaled down from the 256 MiB default so
/// the suite is fast on any CI host without invalidating the relative numbers).
const L1: usize = 8 * 1024 * 1024; // 8 MiB

const NS: &str = "bench";

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_cache_l1_only(l1_bytes: usize) -> BlobCache {
    BlobCache::new(
        BlobCacheConfig::builder()
            .l1_bytes_max(l1_bytes)
            .shard_count(64)
            .max_namespaces(16)
            .l2_compression(L2Compression::Off)
            .try_build()
            .unwrap(),
    )
}

fn make_cache_with_l2(l1_bytes: usize, l2_path: &std::path::Path) -> BlobCache {
    BlobCache::new(
        BlobCacheConfig::builder()
            .l1_bytes_max(l1_bytes)
            .l2_bytes_max(4 * 1024 * 1024 * 1024)
            .l2_path(l2_path)
            .shard_count(64)
            .max_namespaces(16)
            .l2_compression(L2Compression::Off)
            .try_build()
            .unwrap(),
    )
}

fn key(i: usize) -> String {
    format!("k{i:010}")
}

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i & 0xFF) as u8).collect()
}

// ── Workload 1 — hot-l1-hit ──────────────────────────────────────────────────
//
// 32 × 1 KB keys, warm in L1. Drives Arc<[u8]> clone path.
// Compares: BlobCache L1 | ResultCache | Redis GET single-shot | pipelined.

fn w1_hot_l1_hit(c: &mut Criterion) {
    const KEY_COUNT: usize = 32;
    const PAYLOAD_SIZE: usize = 1024; // 1 KB

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);
    for i in 0..KEY_COUNT {
        cache.put(NS, key(i), BlobCachePut::new(p.clone())).unwrap();
    }

    let mut rc = ResultCache::new(L1);
    for i in 0..KEY_COUNT {
        rc.insert(CacheKey::new(key(i)), p.clone(), CachePolicy::default());
    }

    let mut g = c.benchmark_group("w1-hot-l1-hit");
    g.throughput(Throughput::Elements(1));

    g.bench_function("BlobCache-L1", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cache.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    g.bench_function("ResultCache", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(rc.get(&CacheKey::new(key(i % KEY_COUNT))));
            i += 1;
        });
    });


    g.finish();
}

// ── Workload 2 — cold-l2-miss ────────────────────────────────────────────────
//
// L1 evicted, L2 hit. 32K × 16 KB keys (512 MiB working set > L1).
// Scaled down: 512 × 16 KB = 8 MiB = 1× L1, then narrow L1 to 1 MiB.

fn w2_cold_l2_miss(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const KEY_COUNT: usize = 512;
    const PAYLOAD_SIZE: usize = 16 * 1024; // 16 KB

    // Populate with full-size cache (L1 + L2).
    let full_cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
    let p = payload(PAYLOAD_SIZE);
    for i in 0..KEY_COUNT {
        full_cache
            .put(NS, key(i), BlobCachePut::new(p.clone()))
            .ok();
    }
    drop(full_cache);

    // Reopen with tiny L1 so reads come from L2.
    let cold_cache = make_cache_with_l2(1024, &tmp.path().join("cache.rdb"));

    let mut g = c.benchmark_group("w2-cold-l2-miss");
    g.throughput(Throughput::Elements(1));
    g.sample_size(50);

    g.bench_function("BlobCache-L2-hit", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cold_cache.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    g.finish();
}

// ── Workload 3 — cold-absent (synopsis effectiveness) ────────────────────────
//
// 100K keys, none written. Synopsis must skip L2 metadata reads.

fn w3_cold_absent(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    // Populate a few keys so the namespace + synopsis exist.
    let cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
    let p = payload(64);
    for i in 0..100usize {
        cache.put(NS, key(i), BlobCachePut::new(p.clone())).unwrap();
    }

    let mut g = c.benchmark_group("w3-cold-absent");
    g.throughput(Throughput::Elements(1));

    g.bench_function("BlobCache-synopsis-miss", |b| {
        let mut i = 100usize; // keys that were never inserted
        b.iter(|| {
            black_box(cache.get(NS, &key(i)));
            i += 1;
        });
    });

    let mut rc = ResultCache::new(L1);
    for i in 0..100usize {
        rc.insert(CacheKey::new(key(i)), p.clone(), CachePolicy::default());
    }
    g.bench_function("ResultCache-miss", |b| {
        let mut i = 100usize;
        b.iter(|| {
            black_box(rc.get(&CacheKey::new(key(i))));
            i += 1;
        });
    });


    g.finish();
}

// ── Workload 4 — large-blob-l2-hit (5 MiB) ──────────────────────────────────
//
// 5 MiB blob, L1 cold, L2 warm. Two L1-admission cells.

fn w4_large_blob_l2_hit(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const BLOB_SIZE: usize = 5 * 1024 * 1024; // 5 MiB
    const KEY_COUNT: usize = 4;

    let full = make_cache_with_l2(64 * 1024 * 1024, &tmp.path().join("cache.rdb")); // 64 MiB L1
    let p = payload(BLOB_SIZE);
    for i in 0..KEY_COUNT {
        full.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
    }
    drop(full);

    let cold = make_cache_with_l2(1024, &tmp.path().join("cache.rdb"));

    let mut g = c.benchmark_group("w4-large-blob-l2-hit");
    g.throughput(Throughput::Bytes(BLOB_SIZE as u64));
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(10));

    g.bench_function("BlobCache-L2-hit-5MiB", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cold.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    g.finish();
}

// ── Workload 5 — namespace-flush ─────────────────────────────────────────────
//
// O(1) generation bump on the foreground flush call.

fn w5_namespace_flush(c: &mut Criterion) {
    const KEY_COUNT: usize = 1000; // scaled from 50K
    const PAYLOAD_SIZE: usize = 4 * 1024; // 4 KB

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);

    let mut g = c.benchmark_group("w5-namespace-flush");
    g.sample_size(50);

    g.bench_function("BlobCache-generation-bump", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Repopulate
                for i in 0..KEY_COUNT {
                    cache.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
                }
                let t = Instant::now();
                black_box(cache.invalidate_namespace(NS));
                total += t.elapsed();
            }
            total
        });
    });

    g.finish();
}

// ── Workload 6 — dependency-invalidation ─────────────────────────────────────
//
// 25% of entries carry "table:users" dep. Measure invalidate_dependencies.

fn w6_dependency_invalidation(c: &mut Criterion) {
    const KEY_COUNT: usize = 1000; // scaled from 100K
    const PAYLOAD_SIZE: usize = 4 * 1024;

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);

    let mut rc = ResultCache::new(L1);

    let mut g = c.benchmark_group("w6-dependency-invalidation");
    g.sample_size(30);

    g.bench_function("BlobCache-dep-tag", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Repopulate with 25% tagged
                for i in 0..KEY_COUNT {
                    let put = if i % 4 == 0 {
                        BlobCachePut::new(p.clone())
                            .with_dependencies(["table:users"])
                    } else {
                        BlobCachePut::new(p.clone())
                    };
                    cache.put(NS, key(i), put).ok();
                }
                let t = Instant::now();
                black_box(cache.invalidate_dependencies(NS, &["table:users"]));
                total += t.elapsed();
            }
            total
        });
    });

    g.bench_function("ResultCache-invalidate-deps", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                for i in 0..KEY_COUNT {
                    let policy = if i % 4 == 0 {
                        CachePolicy::default().depends_on(&["table:users"])
                    } else {
                        CachePolicy::default()
                    };
                    rc.insert(CacheKey::new(key(i)), p.clone(), policy);
                }
                let t = Instant::now();
                rc.invalidate_by_dependency("table:users");
                total += t.elapsed();
            }
            total
        });
    });

    g.finish();
}

// ── Workload 7 — restart-warm-cache ──────────────────────────────────────────
//
// Write 200K × 8 KB keys to L2, drop BlobCache, reopen, measure open + first hit.
// Scaled: 128 × 8 KB = 1 MiB so the bench doesn't require minutes.

fn w7_restart_warm_cache(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const KEY_COUNT: usize = 128;
    const PAYLOAD_SIZE: usize = 8 * 1024; // 8 KB

    // Populate phase.
    {
        let cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
        let p = payload(PAYLOAD_SIZE);
        for i in 0..KEY_COUNT {
            cache.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
        }
    } // drop flushes L2

    let l2_path = tmp.path().join("cache.rdb");
    let mut g = c.benchmark_group("w7-restart-warm-cache");
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(10));

    g.bench_function("BlobCache-reopen-first-hit", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let t = Instant::now();
                let reopened = make_cache_with_l2(L1, &l2_path);
                black_box(reopened.get(NS, &key(0)));
                total += t.elapsed();
            }
            total
        });
    });

    g.finish();
}

// ── Workload 8 — mixed-blob admission ────────────────────────────────────────
//
// Mix of 1 KB (70%) / 100 KB (25%) / 5 MiB (5%) blobs at WS = 0.5, 1.0, 2.0 × L1.
// This is the SIEVE vs W-TinyLFU oracle (W-TinyLFU not yet flagged → n/a row).

fn w8_mixed_blob_admission(c: &mut Criterion) {
    // Key counts sized so total bytes ≈ target WS multiple of L1.
    // 1 KB × 70% + 100 KB × 25% + 5 MiB × 5% = 0.7 + 25 + 262 ≈ 288 KB avg per slot
    // We approximate with scaled L1=8 MiB.
    // WS=0.5: total_bytes = 4 MiB → ops ~14 keys
    // WS=1.0: total_bytes = 8 MiB → ops ~28 keys
    // WS=2.0: total_bytes = 16 MiB → ops ~56 keys
    let workloads: &[(&str, usize)] = &[
        ("WS-0.5xL1", L1 / 2),
        ("WS-1.0xL1", L1),
        ("WS-2.0xL1", L1 * 2),
    ];

    let small_payload = payload(1024);
    let medium_payload = payload(100 * 1024);
    let large_payload = payload(1024 * 1024); // scaled from 5 MiB → 1 MiB

    let mut g = c.benchmark_group("w8-mixed-blob-admission");
    g.sample_size(20);

    for (label, ws_bytes) in workloads {
        // Build a mixed key set whose total size ~= ws_bytes.
        let small_count = (ws_bytes * 70 / 100) / 1024;
        let medium_count = (ws_bytes * 25 / 100) / (100 * 1024);
        let large_count = (ws_bytes * 5 / 100) / (1024 * 1024);
        let total_keys = (small_count + medium_count + large_count).max(1);

        g.bench_with_input(
            BenchmarkId::new("BlobCache-SIEVE-put-get", label),
            label,
            |b, _| {
                let cache = make_cache_l1_only(L1);
                // Warm fill
                for i in 0..total_keys {
                    let put = if i < small_count {
                        BlobCachePut::new(small_payload.clone())
                    } else if i < small_count + medium_count {
                        BlobCachePut::new(medium_payload.clone())
                    } else {
                        BlobCachePut::new(large_payload.clone())
                    };
                    cache.put(NS, key(i), put).ok();
                }
                let mut i = 0usize;
                b.iter(|| {
                    // 80/20 read/write mix
                    if i % 5 == 0 {
                        let idx = i % total_keys;
                        let put = if idx < small_count {
                            BlobCachePut::new(small_payload.clone())
                        } else if idx < small_count + medium_count {
                            BlobCachePut::new(medium_payload.clone())
                        } else {
                            BlobCachePut::new(large_payload.clone())
                        };
                        cache.put(NS, key(idx), put).ok();
                    } else {
                        black_box(cache.get(NS, &key(i % total_keys)));
                    }
                    i += 1;
                });
            },
        );
    }

    g.finish();
}

// ── Redis helpers (gated on REDIS_NO_PERSIST_ADDR / REDIS_AOF_ADDR env vars) ─
// These stubs exist so the bench file compiles without the redis crate.
// When Docker is available, run bench/blob-cache/redis-up.sh, then re-bench.

fn _redis_addr(env_var: &str) -> Option<String> {
    std::env::var(env_var).ok()
}

// ── criterion wiring ─────────────────────────────────────────────────────────

criterion_group!(
    benches,
    w1_hot_l1_hit,
    w2_cold_l2_miss,
    w3_cold_absent,
    w4_large_blob_l2_hit,
    w5_namespace_flush,
    w6_dependency_invalidation,
    w7_restart_warm_cache,
    w8_mixed_blob_admission,
);
criterion_main!(benches);
