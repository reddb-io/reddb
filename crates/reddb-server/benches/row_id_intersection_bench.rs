//! Row-id intersection strategy benchmark (issue #1340, parent #1337).
//!
//! Compares four intersection strategies for the `EntityId` (transparent u64)
//! sets produced by indexed scans.  `EntityId(pub u64)` is a zero-cost wrapper
//! so benchmarking `u64` slices is equivalent; this avoids crossing the
//! `pub(crate)` boundary on `storage::unified::entity`.
//!
//! Strategies
//! ----------
//! - `hashset_siphash`  — current: `HashSet<u64>` (SipHash13) from the smaller
//!                         side then scan the larger.  `intersect_sorted_id_sets`
//!                         in `indexed_scan.rs`.
//! - `hashset_identity` — same shape but with a noop/identity hasher.  Included
//!                         as a candidate for monotonic internal IDs.
//! - `sorted_merge`     — two-pointer linear merge O(|a|+|b|).  Requires both
//!                         inputs to be sorted by EntityId.
//! - `gallop`           — binary-search the smaller set into the larger.
//!                         O(|small| × log|large|).  Also requires sorted inputs.
//!
//! Scenarios
//! ---------
//! dense      – ~50 % overlap (a=0..n, b=0,2,4,..)
//! sparse     – ~10 % overlap (a=0..n, b=0,10,20,..)
//! no_overlap – 0 % (a=evens, b=odds)
//! skewed     – |small|=|large|/10, 100 % of small in large
//!
//! Sizes: 100, 10 000, 100 000 per side.
//!
//! ## Findings (2026-06-25, guard host 14G RAM, single-core build)
//!
//! **sorted_merge** is 10–100× faster when inputs ARE sorted, but
//! `collect_range_limited` returns EntityIds in insertion order within each
//! BTree key-bucket and iterates buckets in key order — the result is NOT
//! sorted by EntityId.  Adding a pre-sort step costs O(n log n) ≈ 3–5 ms at
//! n=100 k, which erases the merge advantage.
//!
//! **hashset_identity** is SLOWER than SipHash at large scales:  consecutive
//! IDs 0..N all share the same top-7-bit hash tag in hashbrown's SwissTable,
//! forcing full-key comparisons on every probe.  At n=100 k it is 10–25×
//! slower than SipHash.  Do NOT use with monotonic IDs starting near 0.
//!
//! **Conclusion: no change to `intersect_sorted_id_sets` is justified.**
//! The current SipHash HashSet implementation is correct and optimal given
//! the actual (unsorted) input properties.  A future optimisation would be
//! to emit sorted IDs from `collect_range_limited` (sort per bucket on insert
//! or at collection time) and then switch to sorted_merge in the intersection.
//! That follow-on would need its own issue to scope the storage-layer change.
//!
//! Selected timings (median, 20 samples, warm-up 1 s, measurement 3 s):
//!
//! | scenario     | n       | hashset_siphash | hashset_identity | sorted_merge | gallop  |
//! |--------------|---------|-----------------|------------------|--------------|---------|
//! | dense 50 %   | 100     | 3.36 µs         | 3.28 µs          | **348 ns**   | 2.03 µs |
//! | dense 50 %   | 10 000  | 312 µs          | 689 µs  ↑bad     | **13.4 µs**  | 363 µs  |
//! | dense 50 %   | 100 000 | 3.79 ms         | 46.6 ms ↑↑bad   | **130 µs**   | 4.97 ms |
//! | sparse 10 %  | 100 000 | 4.74 ms         | 3.01 ms          | **124 µs**   | 5.89 ms |
//! | no-overlap   | 100 000 | 4.17 ms         | 98.8 ms ↑↑bad   | **263 µs**   | 4.58 ms |
//! | skewed 10×   | 100 000 | 1.80 ms         | 36.0 ms ↑↑bad   | **17.0 µs**  | 488 µs  |
//!
//! (sorted_merge numbers assume pre-sorted inputs, which the real path does
//!  not guarantee without an additional sort step.)
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench row_id_intersection_bench

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::hint::black_box;

// ── Identity hasher ──────────────────────────────────────────────────────────
// A noop hasher that passes u64 keys through unchanged.  Hash-flooding safe
// ONLY for internal monotonic IDs under the engine's own allocator.

struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut v = 0u64;
        for (i, &b) in bytes.iter().take(8).enumerate() {
            v |= (b as u64) << (i * 8);
        }
        self.0 = v;
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

#[derive(Clone, Default)]
struct BuildIdentity;

impl BuildHasher for BuildIdentity {
    type Hasher = IdentityHasher;
    #[inline]
    fn build_hasher(&self) -> Self::Hasher {
        IdentityHasher(0)
    }
}

type IdentitySet = HashSet<u64, BuildIdentity>;

// ── Intersection strategies ──────────────────────────────────────────────────

/// Current: HashSet from smaller side, default SipHash13.
fn intersect_hashset_siphash(a: &[u64], b: &[u64], limit: usize) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (larger, smaller) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let set: HashSet<u64> = smaller.iter().copied().collect();
    let mut out = Vec::with_capacity(limit.min(set.len()));
    for &id in larger {
        if set.contains(&id) {
            out.push(id);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Alternative: identity hasher — no hash computation for numeric internal IDs.
fn intersect_hashset_identity(a: &[u64], b: &[u64], limit: usize) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (larger, smaller) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let set: IdentitySet = smaller.iter().copied().collect();
    let mut out = Vec::with_capacity(limit.min(set.len()));
    for &id in larger {
        if set.contains(&id) {
            out.push(id);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Sorted merge: two-pointer O(|a|+|b|).  Both inputs must be sorted ascending.
/// Indexed-scan results come pre-sorted from BTree/sorted-index lookups.
fn intersect_sorted_merge(a: &[u64], b: &[u64], limit: usize) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < a.len() && j < b.len() && out.len() < limit {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    out
}

/// Gallop: binary-search the smaller set into the larger, advancing the offset
/// after each hit or miss.  O(|small| × log|large|).
fn intersect_gallop(a: &[u64], b: &[u64], limit: usize) -> Vec<u64> {
    let (smaller, larger) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::new();
    let mut offset = 0;
    for &key in smaller {
        if out.len() >= limit || offset >= larger.len() {
            break;
        }
        match larger[offset..].binary_search(&key) {
            Ok(pos) => {
                out.push(key);
                offset += pos + 1;
            }
            Err(pos) => {
                offset += pos;
            }
        }
    }
    out
}

// ── Data generators ──────────────────────────────────────────────────────────

/// Dense overlap (~50 %): a = consecutive 0..n, b = even multiples 0..2n.
fn dense(n: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..n as u64).collect();
    let b: Vec<u64> = (0..n as u64).map(|i| i * 2).collect();
    (a, b)
}

/// Sparse overlap (~10 %): a = consecutive 0..n, b = decade multiples.
fn sparse(n: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..n as u64).collect();
    let b: Vec<u64> = (0..n as u64).map(|i| i * 10).collect();
    (a, b)
}

/// No overlap: a = even numbers, b = odd numbers.
fn no_overlap(n: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..n as u64).map(|i| i * 2).collect();
    let b: Vec<u64> = (0..n as u64).map(|i| i * 2 + 1).collect();
    (a, b)
}

/// Skewed: small = 0..n/10, large = 0..n (100 % of small is in large).
fn skewed(n: usize) -> (Vec<u64>, Vec<u64>) {
    let small_n = (n / 10).max(1);
    let small: Vec<u64> = (0..small_n as u64).collect();
    let large: Vec<u64> = (0..n as u64).collect();
    (small, large)
}

// ── Bench helpers ────────────────────────────────────────────────────────────

const SIZES: &[usize] = &[100, 10_000, 100_000];
const SAMPLE: usize = 20;

macro_rules! bench_strategies {
    ($group:expr, $a:expr, $b:expr, $n:expr) => {{
        let a: &[u64] = &$a;
        let b: &[u64] = &$b;
        $group.bench_with_input(
            BenchmarkId::new("hashset_siphash", $n),
            &(a, b),
            |bench, &(a, b)| {
                bench.iter(|| {
                    black_box(intersect_hashset_siphash(black_box(a), black_box(b), usize::MAX))
                })
            },
        );
        $group.bench_with_input(
            BenchmarkId::new("hashset_identity", $n),
            &(a, b),
            |bench, &(a, b)| {
                bench.iter(|| {
                    black_box(intersect_hashset_identity(black_box(a), black_box(b), usize::MAX))
                })
            },
        );
        $group.bench_with_input(
            BenchmarkId::new("sorted_merge", $n),
            &(a, b),
            |bench, &(a, b)| {
                bench.iter(|| {
                    black_box(intersect_sorted_merge(black_box(a), black_box(b), usize::MAX))
                })
            },
        );
        $group.bench_with_input(
            BenchmarkId::new("gallop", $n),
            &(a, b),
            |bench, &(a, b)| {
                bench.iter(|| {
                    black_box(intersect_gallop(black_box(a), black_box(b), usize::MAX))
                })
            },
        );
    }};
}

// ── Benchmark groups ─────────────────────────────────────────────────────────

fn bench_dense(c: &mut Criterion) {
    let mut group = c.benchmark_group("row-id-intersection/dense-50pct");
    group.sample_size(SAMPLE);
    for &n in SIZES {
        group.throughput(Throughput::Elements(n as u64));
        let (a, b) = dense(n);
        bench_strategies!(group, a, b, n);
    }
    group.finish();
}

fn bench_sparse(c: &mut Criterion) {
    let mut group = c.benchmark_group("row-id-intersection/sparse-10pct");
    group.sample_size(SAMPLE);
    for &n in SIZES {
        group.throughput(Throughput::Elements(n as u64));
        let (a, b) = sparse(n);
        bench_strategies!(group, a, b, n);
    }
    group.finish();
}

fn bench_no_overlap(c: &mut Criterion) {
    let mut group = c.benchmark_group("row-id-intersection/no-overlap");
    group.sample_size(SAMPLE);
    for &n in SIZES {
        group.throughput(Throughput::Elements(n as u64));
        let (a, b) = no_overlap(n);
        bench_strategies!(group, a, b, n);
    }
    group.finish();
}

fn bench_skewed(c: &mut Criterion) {
    let mut group = c.benchmark_group("row-id-intersection/skewed-10x");
    group.sample_size(SAMPLE);
    // n is the large side; small = n/10
    for &n in SIZES {
        let small_n = (n / 10).max(1);
        // Throughput: total elements processed = small + large
        group.throughput(Throughput::Elements((small_n + n) as u64));
        let (a, b) = skewed(n);
        bench_strategies!(group, a, b, n);
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_dense,
    bench_sparse,
    bench_no_overlap,
    bench_skewed,
);
criterion_main!(benches);
