//! Cache-ring contention benchmark (issue #1343, parent #1337).
//!
//! Measures whether the `BufferRing` lock structure is a *real*
//! performance problem under concurrent access, BEFORE changing its
//! synchronization shape.  The ring is the small circular cache used by
//! `BufferAccessStrategy` for sequential scans and bulk read/write — see
//! `storage/cache/ring.rs`.
//!
//! ## What is being measured
//!
//! `BufferRing` guards its state with two `RwLock`s (`slots` + `map`) plus
//! an `AtomicUsize` hand:
//!
//!   - `get`     takes a read lock on `map`, then a read lock on `slots`.
//!   - `insert`  in the common (not-already-present) path takes a *write*
//!     lock on BOTH `map` and `slots` for the whole eviction / placement
//!     sweep.
//!
//! So concurrent readers can proceed in parallel, but any insert
//! serialises every other operation on the ring.  The question this slice
//! answers is: does that serialisation actually hurt at the contention
//! levels the ring sees, enough to justify a synchronization rewrite
//! (e.g. sharding, lock-free slots, or a striped hand)?
//!
//! ## Method
//!
//! For each (workload, capacity, thread-count) we spawn N threads that each
//! hammer a single shared `Arc<BufferRing>` with a fixed op budget, and time
//! the whole batch with `iter_custom`.  Reporting *total* ops/batch across a
//! growing thread count exposes contention directly:
//!
//!   - throughput scales ~linearly with threads  → lock is NOT a bottleneck
//!   - throughput stays flat or *drops* as threads grow → contention is real
//!
//! Three workloads bracket the realistic mix:
//!
//!   - `read_heavy`  95 % get / 5 % insert  — steady-state scan re-reads
//!   - `mixed`       50 % get / 50 % insert — scan fill + revisit
//!   - `write_heavy` 5 % get / 95 % insert  — bulk-load fill (worst case)
//!
//! Capacities 16 and 32 mirror `SequentialScan` (16) and `BulkRead/
//! BulkWrite` (32) in `strategy.rs`.
//!
//! ## Findings (2026-06-25, guard host, 14G RAM, --measurement-time 3)
//!
//! Aggregate throughput (median, Melem/s) — total ops/sec across ALL threads.
//! With a non-contended lock this column would rise with thread count; here it
//! *falls* — adding threads makes the ring slower in absolute terms.
//!
//! | workload     | cap | 1 thr | 2 thr | 4 thr | 8 thr | 1→8 scaling |
//! |--------------|-----|-------|-------|-------|-------|-------------|
//! | read-heavy   | 16  | 29.5  | 8.54  | 7.24  | 6.04  | 0.20×       |
//! | read-heavy   | 32  | 27.3  | 8.34  | 7.44  | 6.10  | 0.22×       |
//! | mixed-50/50  | 16  | 17.0  | 3.66  | 2.63  | 1.19  | 0.07×       |
//! | mixed-50/50  | 32  | 17.8  | 4.27  | 2.62  | 1.18  | 0.07×       |
//! | write-heavy  | 16  | 14.4  | 3.40  | 1.76  | 1.65  | 0.11×       |
//! | write-heavy  | 32  | 14.6  | 3.21  | 1.79  | 0.74  | 0.05×       |
//!
//! Ideal linear scaling at 8 threads would be 8.0×.  Observed scaling is
//! 0.05–0.22× — i.e. *negative*: total throughput at 8 threads is 5–20× LOWER
//! than a single thread.  This is the textbook signature of a single hot lock:
//! threads spend their time contending for and bouncing the `RwLock` cache
//! lines rather than doing work.  Even the read-heavy workload degrades badly,
//! because every `get` acquires two read locks in sequence and the 5 % of
//! inserts take exclusive write locks that stall all readers; the lock-word
//! cache-line ping-pong dominates regardless.
//!
//! **Conclusion: lock contention IS material.**  The current two-`RwLock`
//! design does not scale under concurrent access at any of the realistic
//! workload mixes or ring capacities measured.  A synchronization rewrite is
//! justified — candidate directions for a follow-up *implementation* slice:
//!
//!   - shard the ring into independent striped sub-rings keyed by `hash(key)`
//!     so concurrent ops touch disjoint locks;
//!   - collapse `map` + `slots` under a single lock (they are always taken
//!     together on the insert path anyway) to halve acquisition cost;
//!   - or a lock-free / sharded-`Mutex` slot table if FIFO-hand semantics can
//!     be relaxed to per-shard hands (must preserve eviction semantics).
//!
//! IMPORTANT CAVEAT: this benchmark drives one *shared* ring with up to 8
//! threads.  In production each `BufferAccessStrategy` ring is created per
//! scan/cursor and is usually touched by a single executor thread, so the
//! real-world contention depends on how many concurrent scans share a ring.
//! The follow-up implementation slice should first confirm that rings are in
//! fact shared across threads on a hot path before investing in sharding; if
//! they are per-thread in practice, the contention measured here is latent,
//! not active, and the rewrite can be deferred.  Either way, the evidence
//! here is strong enough to justify scoping that follow-up.
//!
//! This slice changes NO cache logic or eviction semantics — it only adds the
//! measurement.  The existing `ring.rs` unit tests remain the behaviour gate.
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench cache_ring_contention_bench

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use reddb_server::storage::cache::ring::BufferRing;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Ops each thread performs per batch iteration.
const OPS_PER_THREAD: u64 = 50_000;

/// Thread counts swept to expose scaling behaviour.
const THREADS: &[usize] = &[1, 2, 4, 8];

/// Representative ring capacities (SequentialScan=16, BulkRead/Write=32).
const CAPACITIES: &[usize] = &[16, 32];

/// One-line conclusion, filled in from the recorded findings.  Kept as a
/// `const` so the verdict lives in the source next to the benchmark that
/// produced it.
const _CONCLUSION: &str = "See module doc comment § Findings for the verdict \
    on whether a BufferRing synchronization rewrite is justified.";

/// A cheap, allocation-free per-thread RNG (xorshift) so the benchmark
/// measures lock behaviour, not allocator or hash noise.  Seeded distinctly
/// per thread so threads touch different keys.
#[inline]
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Run `threads` workers against one shared ring, each doing `OPS_PER_THREAD`
/// operations with the given get-probability (0..=100).  Returns the elapsed
/// wall-clock for the whole batch.
fn run_batch(threads: usize, capacity: usize, get_pct: u64, key_space: u64) -> Duration {
    // Pre-fill the ring so reads can hit and inserts mostly evict — this is
    // the steady state we care about, not a cold ring.
    let ring: Arc<BufferRing<u64, u64>> = Arc::new(BufferRing::new(capacity));
    for k in 0..capacity as u64 {
        ring.insert(k, k);
    }

    let start = Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let ring = Arc::clone(&ring);
            scope.spawn(move || {
                // Distinct seed per thread; never zero (xorshift fixed point).
                let mut state = (t as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                for _ in 0..OPS_PER_THREAD {
                    let r = next_rand(&mut state);
                    let key = r % key_space;
                    if r % 100 < get_pct {
                        black_box(ring.get(&key));
                    } else {
                        black_box(ring.insert(key, key));
                    }
                }
            });
        }
    });
    start.elapsed()
}

fn bench_workload(c: &mut Criterion, name: &str, get_pct: u64) {
    let mut group = c.benchmark_group(format!("cache-ring-contention/{name}"));
    group.sample_size(20);
    for &capacity in CAPACITIES {
        // Key space a few× capacity so inserts both hit (update-in-place) and
        // miss (evict), exercising both lock paths.
        let key_space = (capacity as u64) * 4;
        for &threads in THREADS {
            let total_ops = OPS_PER_THREAD * threads as u64;
            group.throughput(criterion::Throughput::Elements(total_ops));
            group.bench_with_input(
                BenchmarkId::new(format!("cap{capacity}"), threads),
                &threads,
                |b, &threads| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::ZERO;
                        for _ in 0..iters {
                            total += run_batch(threads, capacity, get_pct, key_space);
                        }
                        total
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_read_heavy(c: &mut Criterion) {
    bench_workload(c, "read-heavy-95g", 95);
}

fn bench_mixed(c: &mut Criterion) {
    bench_workload(c, "mixed-50g", 50);
}

fn bench_write_heavy(c: &mut Criterion) {
    bench_workload(c, "write-heavy-5g", 5);
}

criterion_group!(benches, bench_read_heavy, bench_mixed, bench_write_heavy);
criterion_main!(benches);
