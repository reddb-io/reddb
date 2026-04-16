//! Global temporal index over timeseries chunks.
//!
//! Today each [`super::chunk::TimeSeriesChunk`] tracks its own
//! `min_timestamp` / `max_timestamp`, but there is no structure above the
//! chunk set that answers "which chunks overlap `[start, end]`?" efficiently.
//! Callers fall back to a linear scan over every chunk.
//!
//! This module introduces [`TemporalIndex`] — a `BTreeMap` keyed by each
//! chunk's `min_ts`, paired with a bloom filter of registered chunk start
//! timestamps. It powers:
//!
//! - `chunks_overlapping(start, end)` → O(log n + k) interval probe
//! - `chunks_at_timestamp(ts)`        → point lookup
//! - cross-structure **temporal join** (tables ↔ timeseries) once the
//!   planner can ask "give me chunks around this row's event_ts"
//!
//! The index stores opaque [`ChunkHandle`]s — callers decide what `chunk_id`
//! and `series_id` mean. That keeps this module independent of the chunk
//! lifecycle (growing/sealed/flushed) and the on-disk layout.
//!
//! Implements [`crate::storage::index::IndexBase`] and exposes a bloom via
//! [`crate::storage::index::HasBloom`] so the query planner and segment
//! layer can prune uniformly.

use std::collections::{BTreeMap, HashSet};
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::storage::index::{BloomSegment, HasBloom, IndexBase, IndexKind, IndexStats};

/// Number of chunks grouped into a single BRIN block range.
/// Coarser granularity = fewer range entries, faster pre-filter at the cost
/// of reduced precision (may read slightly more chunks than necessary).
/// Equivalent to PostgreSQL's `pages_per_range` parameter.
const BRIN_CHUNKS_PER_RANGE: usize = 128;

/// A BRIN block range: summarises min/max timestamp across up to
/// `BRIN_CHUNKS_PER_RANGE` consecutive chunks. Query planning skips an
/// entire range in O(1) when the range's `[min_ts, max_ts]` does not
/// intersect the query window — identical to PostgreSQL's BRIN MINMAX scan.
#[derive(Debug, Clone, Copy)]
struct BrinRange {
    /// Minimum timestamp across all chunks in this range.
    min_ts: u64,
    /// Maximum timestamp across all chunks in this range.
    max_ts: u64,
    /// How many chunk handles fall in this range (≤ BRIN_CHUNKS_PER_RANGE).
    chunk_count: usize,
}

impl BrinRange {
    #[inline]
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.max_ts >= start && self.min_ts <= end
    }
}

/// Opaque handle describing a chunk the index tracks.
///
/// Callers are free to interpret `series_id`/`chunk_id`. Only `min_ts` and
/// `max_ts` are consulted for query planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkHandle {
    /// Opaque identifier for the series (metric + tag combination hash, for
    /// example). Passed through unchanged.
    pub series_id: u64,
    /// Opaque identifier for the chunk within its series.
    pub chunk_id: u64,
    /// Earliest timestamp present in the chunk, inclusive.
    pub min_ts: u64,
    /// Latest timestamp present in the chunk, inclusive.
    pub max_ts: u64,
}

impl ChunkHandle {
    /// Returns true iff the handle's `[min_ts, max_ts]` interval intersects
    /// `[start, end]` (all inclusive).
    #[inline]
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        self.max_ts >= start && self.min_ts <= end
    }

    /// Returns true iff `ts` falls within the handle's interval.
    #[inline]
    pub fn contains(&self, ts: u64) -> bool {
        ts >= self.min_ts && ts <= self.max_ts
    }
}

/// Internal state grouped under a single lock so `register()` takes one
/// write-lock instead of three. BTree, BRIN ranges, count, and correlation
/// stats are always updated together — splitting them only added contention.
struct IndexState {
    /// BTree keyed by `min_ts`. Multiple chunks may share the same `min_ts`,
    /// so each key maps to a `Vec`.
    entries: BTreeMap<u64, Vec<ChunkHandle>>,
    /// BRIN block-range summaries. Last entry is the "open" range being
    /// filled; earlier entries are sealed with `BRIN_CHUNKS_PER_RANGE` chunks.
    brin_ranges: Vec<BrinRange>,
    /// Number of registered handles. Decremented on `unregister`.
    count: usize,
    /// Number of monotonic insertions: `handle.min_ts >= prev_max_min_ts`.
    /// Used to compute `index_correlation` — PostgreSQL's BRIN planner cost
    /// model relies on this to decide whether BRIN is worth using.
    monotonic_inserts: u64,
    /// Total `register()` calls — denominator for correlation.
    total_inserts: u64,
    /// Highest `min_ts` seen so far (for monotonic check on next insert).
    last_max_min_ts: u64,
}

/// Temporal BTree index keyed by `min_ts`, with a BRIN block-range layer.
///
/// Two-level structure mirrors PostgreSQL's BRIN architecture:
///
/// **Level 1 — BRIN block ranges**: each entry summarises the min/max
/// timestamps of up to `BRIN_CHUNKS_PER_RANGE` consecutive registered
/// chunks. A query first scans this tiny array (O(R/N) where R = chunks,
/// N = BRIN_CHUNKS_PER_RANGE) and skips entire blocks that cannot intersect
/// the query window.
///
/// **Level 2 — BTree**: keyed by `min_ts`, provides the precise
/// O(log n + k) probe for surviving blocks.
///
/// **Locking strategy:** the BTree, BRIN ranges, count, and correlation
/// stats live under a single `IndexState` lock so `register()` takes one
/// write-lock per insert. Bloom has its own lock (separable write pattern),
/// `global_max` is `AtomicU64` so unbounded range queries never block.
pub struct TemporalIndex {
    state: parking_lot::RwLock<IndexState>,
    /// Bloom filter over registered `min_ts` values. Cheap negative check
    /// before touching the BTree.
    bloom: parking_lot::RwLock<BloomSegment>,
    /// Highest `max_ts` seen so far. Atomic — lock-free for unbounded range
    /// queries ("everything after T").
    global_max: AtomicU64,
}

impl TemporalIndex {
    /// Create an empty index sized for `expected_chunks` entries.
    pub fn new(expected_chunks: usize) -> Self {
        Self {
            state: parking_lot::RwLock::new(IndexState {
                entries: BTreeMap::new(),
                brin_ranges: Vec::new(),
                count: 0,
                monotonic_inserts: 0,
                total_inserts: 0,
                last_max_min_ts: 0,
            }),
            bloom: parking_lot::RwLock::new(BloomSegment::with_capacity(expected_chunks.max(1024))),
            global_max: AtomicU64::new(0),
        }
    }

    /// Register a chunk handle. Safe to call from multiple threads.
    ///
    /// Single state write-lock + bloom lock + atomic CAS for `global_max`.
    /// Updates BTree, BRIN block-range summary, count, and correlation
    /// tracking atomically.
    pub fn register(&self, handle: ChunkHandle) {
        // Single state write-lock — covers BTree, BRIN ranges, count, correlation.
        {
            let mut s = self.state.write();
            s.entries
                .entry(handle.min_ts)
                .or_default()
                .push(handle);
            s.count += 1;

            // Correlation tracking: monotonic if min_ts didn't go backwards.
            s.total_inserts += 1;
            if handle.min_ts >= s.last_max_min_ts {
                s.monotonic_inserts += 1;
            }
            if handle.min_ts > s.last_max_min_ts {
                s.last_max_min_ts = handle.min_ts;
            }

            // BRIN block-range update: widen open range, seal when full.
            if let Some(last) = s.brin_ranges.last_mut() {
                if handle.min_ts < last.min_ts {
                    last.min_ts = handle.min_ts;
                }
                if handle.max_ts > last.max_ts {
                    last.max_ts = handle.max_ts;
                }
                last.chunk_count += 1;
                if last.chunk_count >= BRIN_CHUNKS_PER_RANGE {
                    s.brin_ranges.push(BrinRange {
                        min_ts: u64::MAX,
                        max_ts: 0,
                        chunk_count: 0,
                    });
                }
            } else {
                s.brin_ranges.push(BrinRange {
                    min_ts: handle.min_ts,
                    max_ts: handle.max_ts,
                    chunk_count: 1,
                });
            }
        }

        // Bloom: separate lock (different read/write pattern).
        self.bloom.write().insert(&handle.min_ts.to_le_bytes());

        // Global max via atomic CAS — lock-free for unbounded range queries.
        let mut current = self.global_max.load(Ordering::Relaxed);
        while handle.max_ts > current {
            match self.global_max.compare_exchange_weak(
                current,
                handle.max_ts,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Forget every handle with the given `chunk_id`. Does not touch the
    /// bloom (bloom filters don't support removal — stale positives cost
    /// at most an extra BTree probe that finds no match).
    ///
    /// BRIN ranges are NOT reconstructed on unregister (no desummarization,
    /// same as PostgreSQL). Ranges may become slightly over-wide after
    /// removals — acceptable, false positives only add a cheap BTree probe.
    pub fn unregister(&self, chunk_id: u64) -> usize {
        let mut removed = 0usize;
        let mut s = self.state.write();
        s.entries.retain(|_, handles| {
            let before = handles.len();
            handles.retain(|h| h.chunk_id != chunk_id);
            removed += before - handles.len();
            !handles.is_empty()
        });
        if removed > 0 {
            s.count = s.count.saturating_sub(removed);
        }
        removed
    }

    /// Return every handle whose interval overlaps `[start, end]` (inclusive).
    ///
    /// Two-phase scan — mirrors PostgreSQL's BRIN scan path:
    ///
    /// **Phase 1 (BRIN pre-filter):** iterate the coarse block-range array.
    /// Skip any block whose `[min_ts, max_ts]` does not intersect `[start, end]`.
    /// For append-only workloads this prunes the majority of blocks in O(R/N).
    ///
    /// **Phase 2 (BTree probe):** for each surviving block, scan BTree keys
    /// in `[range.min_ts, min(range.max_ts, end)]` and verify `max_ts >= start`.
    ///
    /// **Dedup:** out-of-order inserts can produce overlapping BRIN ranges
    /// where a BTree entry falls under two surviving windows. We dedup by
    /// `(series_id, chunk_id)` so callers never see duplicates.
    pub fn chunks_overlapping(&self, start: u64, end: u64) -> Vec<ChunkHandle> {
        if start > end {
            return Vec::new();
        }

        let s = self.state.read();
        let mut out = Vec::new();

        if s.brin_ranges.is_empty() {
            // No BRIN ranges yet — plain BTree scan (startup / low-volume path).
            for (_, handles) in s.entries.range((Bound::Unbounded, Bound::Included(end))) {
                for h in handles {
                    if h.max_ts >= start {
                        out.push(*h);
                    }
                }
            }
            return out;
        }

        // Phase 1: collect surviving block windows.
        let mut surviving_windows: Vec<(u64, u64)> = Vec::new();
        for r in s.brin_ranges.iter() {
            if r.chunk_count == 0 {
                continue;
            }
            if r.overlaps(start, end) {
                surviving_windows.push((r.min_ts, r.max_ts));
            }
        }

        if surviving_windows.is_empty() {
            return Vec::new();
        }

        // Phase 2: BTree probe restricted to surviving windows + dedup.
        // Out-of-order inserts can cause overlapping BRIN ranges → same
        // BTree entry hit by multiple probes. Dedup by (series_id, chunk_id).
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        for (win_min, win_max) in surviving_windows {
            let probe_end = win_max.min(end);
            for (_, handles) in
                s.entries.range((Bound::Included(win_min), Bound::Included(probe_end)))
            {
                for h in handles {
                    if h.max_ts >= start && seen.insert((h.series_id, h.chunk_id)) {
                        out.push(*h);
                    }
                }
            }
        }
        out
    }

    /// Return every handle whose interval contains `ts`.
    pub fn chunks_at_timestamp(&self, ts: u64) -> Vec<ChunkHandle> {
        self.chunks_overlapping(ts, ts)
    }

    /// Bloom-backed fast path: returns `false` iff no chunk with
    /// `min_ts == ts` has ever been registered. Useful for dedup checks.
    pub fn min_ts_possibly_registered(&self, ts: u64) -> bool {
        self.bloom.read().contains(&ts.to_le_bytes())
    }

    /// Number of registered chunks.
    pub fn len(&self) -> usize {
        self.state.read().count
    }

    /// Is the index empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Highest `max_ts` seen so far. Cheap upper bound for unbounded range
    /// queries ("everything after T"). Lock-free atomic load.
    pub fn global_max_timestamp(&self) -> u64 {
        self.global_max.load(Ordering::Acquire)
    }

    /// Number of BRIN block ranges (useful for diagnostics / EXPLAIN output).
    pub fn brin_range_count(&self) -> usize {
        self.state
            .read()
            .brin_ranges
            .iter()
            .filter(|r| r.chunk_count > 0)
            .count()
    }

    /// Empirical correlation between insertion order and `min_ts` order,
    /// in `[0.0, 1.0]`. PostgreSQL's planner uses this to decide whether
    /// BRIN is worth using — values near 1.0 mean append-only monotonic
    /// inserts (BRIN very effective), values near 0.0 mean random inserts
    /// (BRIN degrades to a full scan and should be skipped).
    ///
    /// Returns `1.0` for an empty index (matches the historical hardcoded
    /// optimistic default).
    pub fn index_correlation(&self) -> f64 {
        let s = self.state.read();
        if s.total_inserts == 0 {
            1.0
        } else {
            s.monotonic_inserts as f64 / s.total_inserts as f64
        }
    }

    /// Reset the index. Used by tests and deserialize paths.
    pub fn clear(&self) {
        let mut s = self.state.write();
        s.entries.clear();
        s.brin_ranges.clear();
        s.count = 0;
        s.monotonic_inserts = 0;
        s.total_inserts = 0;
        s.last_max_min_ts = 0;
        drop(s);
        *self.bloom.write() = BloomSegment::with_capacity(1024);
        self.global_max.store(0, Ordering::Release);
    }
}

impl Default for TemporalIndex {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl HasBloom for TemporalIndex {
    fn bloom_segment(&self) -> Option<&BloomSegment> {
        // parking_lot RwLock still precludes handing out a raw reference.
        None
    }

    fn definitely_absent(&self, key: &[u8]) -> bool {
        self.bloom.read().definitely_absent(key)
    }
}

impl IndexBase for TemporalIndex {
    fn name(&self) -> &str {
        "timeseries.temporal"
    }

    fn kind(&self) -> IndexKind {
        IndexKind::Temporal
    }

    fn stats(&self) -> IndexStats {
        let s = self.state.read();
        let entries = s.count;
        let distinct_keys = s.entries.len();
        let brin_ranges = s.brin_ranges.iter().filter(|r| r.chunk_count > 0).count();
        let correlation = if s.total_inserts == 0 {
            1.0
        } else {
            s.monotonic_inserts as f64 / s.total_inserts as f64
        };
        IndexStats {
            entries,
            distinct_keys,
            // Each BRIN range: 24 bytes (min_ts u64 + max_ts u64 + count usize).
            // Each BTree entry: ~48 bytes (key u64 + Vec header + pointer).
            approx_bytes: brin_ranges * 24 + distinct_keys * 48,
            kind: IndexKind::Temporal,
            has_bloom: true,
            index_correlation: correlation,
        }
    }

    fn definitely_absent(&self, key_bytes: &[u8]) -> bool {
        <Self as HasBloom>::definitely_absent(self, key_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(series: u64, chunk: u64, min_ts: u64, max_ts: u64) -> ChunkHandle {
        ChunkHandle {
            series_id: series,
            chunk_id: chunk,
            min_ts,
            max_ts,
        }
    }

    #[test]
    fn overlaps_helper() {
        let h = handle(1, 1, 100, 200);
        assert!(h.overlaps(50, 150));
        assert!(h.overlaps(150, 250));
        assert!(h.overlaps(100, 200));
        assert!(h.overlaps(120, 130));
        assert!(!h.overlaps(0, 99));
        assert!(!h.overlaps(201, 300));
        assert!(h.contains(100));
        assert!(h.contains(200));
        assert!(!h.contains(201));
    }

    #[test]
    fn register_and_overlap_query() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 0, 100));
        idx.register(handle(1, 2, 100, 200));
        idx.register(handle(1, 3, 200, 300));
        idx.register(handle(2, 4, 50, 150));

        // Window [120, 180] overlaps chunk 2 (100..200) AND chunk 4
        // (50..150). Chunk 4's max_ts=150 >= 120, so it's a real hit.
        let hits = idx.chunks_overlapping(120, 180);
        let hit_ids: Vec<u64> = hits.iter().map(|h| h.chunk_id).collect();
        assert_eq!(hits.len(), 2);
        assert!(hit_ids.contains(&2));
        assert!(hit_ids.contains(&4));

        // Window spanning multiple chunks
        let hits = idx.chunks_overlapping(80, 220);
        let ids: Vec<u64> = hits.iter().map(|h| h.chunk_id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
        assert!(ids.contains(&4));

        // Window outside everything
        assert!(idx.chunks_overlapping(500, 600).is_empty());
    }

    #[test]
    fn point_lookup() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 1000, 2000));
        idx.register(handle(2, 2, 1500, 3000));

        let at_1800 = idx.chunks_at_timestamp(1800);
        assert_eq!(at_1800.len(), 2);

        let at_2500 = idx.chunks_at_timestamp(2500);
        assert_eq!(at_2500.len(), 1);
        assert_eq!(at_2500[0].chunk_id, 2);

        assert!(idx.chunks_at_timestamp(9999).is_empty());
    }

    #[test]
    fn unregister_removes_handles() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 10, 0, 100));
        idx.register(handle(1, 11, 100, 200));
        assert_eq!(idx.len(), 2);

        let removed = idx.unregister(10);
        assert_eq!(removed, 1);
        assert_eq!(idx.len(), 1);
        assert!(idx.chunks_at_timestamp(50).is_empty());
        assert_eq!(idx.chunks_at_timestamp(150).len(), 1);
    }

    #[test]
    fn bloom_guards_min_ts_lookup() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 5000, 6000));
        assert!(idx.min_ts_possibly_registered(5000));
        // Bloom may false-positive but the BTree lookup returns nothing.
        assert!(idx.chunks_at_timestamp(999_999).is_empty());
    }

    #[test]
    fn global_max_tracks_highest() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 0, 100));
        idx.register(handle(1, 2, 200, 500));
        idx.register(handle(1, 3, 100, 300));
        assert_eq!(idx.global_max_timestamp(), 500);
    }

    #[test]
    fn stats_reflect_registrations() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 0, 10));
        idx.register(handle(1, 2, 0, 20));
        idx.register(handle(1, 3, 100, 200));
        let s = idx.stats();
        assert_eq!(s.entries, 3);
        // Two distinct min_ts keys: 0 and 100
        assert_eq!(s.distinct_keys, 2);
        assert_eq!(s.kind, IndexKind::Temporal);
        assert!(s.has_bloom);
    }

    #[test]
    fn clear_resets() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 0, 100));
        idx.clear();
        assert!(idx.is_empty());
        assert_eq!(idx.global_max_timestamp(), 0);
        assert!(idx.chunks_at_timestamp(50).is_empty());
    }

    #[test]
    fn reversed_range_returns_empty() {
        let idx = TemporalIndex::new(16);
        idx.register(handle(1, 1, 100, 200));
        assert!(idx.chunks_overlapping(500, 100).is_empty());
    }

    #[test]
    fn dedup_overlapping_brin_windows() {
        // Out-of-order inserts that produce two BRIN windows whose [min,max]
        // overlap. Without dedup the same chunk would be returned twice.
        let idx = TemporalIndex::new(16);
        // Force boundary at 128 chunks: register 130 chunks where the 129th
        // has out-of-order min_ts that overlaps the previous range.
        for i in 0..128u64 {
            idx.register(handle(1, i, 1000 + i, 1000 + i + 5));
        }
        // Range 1 is sealed at [1000, 1132]. Range 2 opens.
        idx.register(handle(1, 200, 1050, 1300)); // out-of-order, in range 2
        idx.register(handle(1, 201, 1100, 1400));

        // Query overlaps both ranges; chunk 200 (min_ts=1050) lives in range 2,
        // but range 1 also covers min_ts=1050. Probes for both windows would
        // return it without dedup.
        let hits = idx.chunks_overlapping(1100, 1200);
        let unique: HashSet<_> = hits.iter().map(|h| h.chunk_id).collect();
        assert_eq!(hits.len(), unique.len(), "duplicates leaked through");
    }

    #[test]
    fn correlation_starts_optimistic_then_tracks_inserts() {
        let idx = TemporalIndex::new(16);
        assert_eq!(idx.index_correlation(), 1.0); // empty default

        // Pure monotonic = 1.0
        for i in 0..10u64 {
            idx.register(handle(1, i, i * 100, i * 100 + 50));
        }
        assert!((idx.index_correlation() - 1.0).abs() < 1e-9);

        // Backfill an out-of-order chunk → drop below 1.0.
        idx.register(handle(1, 99, 50, 100));
        let c = idx.index_correlation();
        assert!(c < 1.0 && c > 0.0, "correlation = {c}");
    }

    #[test]
    fn brin_block_seal_boundary() {
        // Verify the chunk at exactly BRIN_CHUNKS_PER_RANGE seals and the
        // next register opens a fresh range correctly initialized.
        let idx = TemporalIndex::new(BRIN_CHUNKS_PER_RANGE * 2);
        for i in 0..BRIN_CHUNKS_PER_RANGE as u64 {
            idx.register(handle(1, i, i * 10, i * 10 + 5));
        }
        assert_eq!(idx.brin_range_count(), 1);

        // Next register opens range 2.
        idx.register(handle(1, 999, 99_999, 100_000));
        assert_eq!(idx.brin_range_count(), 2);

        // Range 2 must be initialized with the new chunk's bounds (not u64::MAX/0).
        let hits = idx.chunks_overlapping(99_999, 100_000);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, 999);
    }

    #[test]
    fn concurrent_register() {
        use std::sync::Arc;
        use std::thread;

        let idx = Arc::new(TemporalIndex::new(1024));
        let mut handles = vec![];
        for t in 0..4u64 {
            let idx_c = Arc::clone(&idx);
            handles.push(thread::spawn(move || {
                for i in 0..100u64 {
                    idx_c.register(handle(t, t * 1000 + i, i * 10, i * 10 + 9));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(idx.len(), 400);
        // At ts=45 every thread's chunk with i=4 contains it (40..49)
        let hits = idx.chunks_at_timestamp(45);
        assert_eq!(hits.len(), 4);
    }
}
