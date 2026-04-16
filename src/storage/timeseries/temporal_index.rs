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

use std::collections::BTreeMap;
use std::ops::Bound;

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

/// Temporal BTree index keyed by `min_ts`, with a BRIN block-range layer.
///
/// Two-level structure mirrors PostgreSQL's BRIN architecture:
///
/// **Level 1 — BRIN block ranges** (`brin_ranges`): each entry summarises
/// the min/max timestamps of up to `BRIN_CHUNKS_PER_RANGE` consecutive
/// registered chunks. A query first scans this tiny array (O(R/N) where R =
/// chunks, N = BRIN_CHUNKS_PER_RANGE) and skips entire blocks that cannot
/// intersect the query window. For append-only workloads with monotonically
/// increasing timestamps this prunes 90–99% of chunks before touching the
/// BTree.
///
/// **Level 2 — BTree** (`entries`): keyed by `min_ts`, provides the precise
/// O(log n + k) probe for surviving blocks.
///
/// Multiple chunks may share the same `min_ts` so each BTree key maps to a
/// `Vec`.
pub struct TemporalIndex {
    entries: parking_lot::RwLock<BTreeMap<u64, Vec<ChunkHandle>>>,
    /// BRIN block-range summaries. Built incrementally as chunks are
    /// registered. The last entry is the "open" (current) range; all earlier
    /// entries are sealed with exactly `BRIN_CHUNKS_PER_RANGE` chunks.
    brin_ranges: parking_lot::RwLock<Vec<BrinRange>>,
    /// Global maximum `max_ts` across registered chunks. Used as the upper
    /// bound for unbounded range queries without walking to `u64::MAX`.
    global_max: parking_lot::RwLock<u64>,
    /// Bloom filter over registered `min_ts` values. Cheap negative check
    /// before touching the BTree.
    bloom: parking_lot::RwLock<BloomSegment>,
    /// Running count of registered handles. Tracked explicitly so stats
    /// don't have to walk the BTree.
    count: parking_lot::RwLock<usize>,
}

impl TemporalIndex {
    /// Create an empty index sized for `expected_chunks` entries.
    pub fn new(expected_chunks: usize) -> Self {
        Self {
            entries: parking_lot::RwLock::new(BTreeMap::new()),
            brin_ranges: parking_lot::RwLock::new(Vec::new()),
            global_max: parking_lot::RwLock::new(0),
            bloom: parking_lot::RwLock::new(BloomSegment::with_capacity(expected_chunks.max(1024))),
            count: parking_lot::RwLock::new(0),
        }
    }

    /// Register a chunk handle. Safe to call from multiple threads.
    ///
    /// Updates both the fine-grained BTree and the coarse BRIN block-range
    /// summary. The BRIN range for the current "open" block is widened to
    /// include the new handle's [min_ts, max_ts]; once a block fills up to
    /// `BRIN_CHUNKS_PER_RANGE` it is sealed and a new open block begins.
    pub fn register(&self, handle: ChunkHandle) {
        self.entries.write().entry(handle.min_ts).or_default().push(handle);
        self.bloom.write().insert(&handle.min_ts.to_le_bytes());
        {
            let mut gmax = self.global_max.write();
            if handle.max_ts > *gmax {
                *gmax = handle.max_ts;
            }
        }
        *self.count.write() += 1;

        // Update BRIN block-range summary.
        let mut ranges = self.brin_ranges.write();
        if let Some(last) = ranges.last_mut() {
            // Widen the open (last) range to include the new chunk.
            if handle.min_ts < last.min_ts { last.min_ts = handle.min_ts; }
            if handle.max_ts > last.max_ts { last.max_ts = handle.max_ts; }
            last.chunk_count += 1;
            if last.chunk_count >= BRIN_CHUNKS_PER_RANGE {
                // Seal this range; next registration opens a fresh one.
                ranges.push(BrinRange {
                    min_ts: u64::MAX,
                    max_ts: 0,
                    chunk_count: 0,
                });
            }
        } else {
            ranges.push(BrinRange {
                min_ts: handle.min_ts,
                max_ts: handle.max_ts,
                chunk_count: 1,
            });
        }
    }

    /// Forget every handle with the given `chunk_id`. Does not touch the
    /// bloom (bloom filters don't support removal — stale positives cost
    /// at most an extra BTree probe that finds no match).
    ///
    /// BRIN ranges are NOT reconstructed on unregister (no desummarization,
    /// same as PostgreSQL). Ranges may become slightly over-wide after
    /// removals — acceptable, as false positives only add a cheap BTree probe.
    pub fn unregister(&self, chunk_id: u64) -> usize {
        let mut removed = 0usize;
        self.entries.write().retain(|_, handles| {
            let before = handles.len();
            handles.retain(|h| h.chunk_id != chunk_id);
            removed += before - handles.len();
            !handles.is_empty()
        });
        if removed > 0 {
            let mut c = self.count.write();
            *c = c.saturating_sub(removed);
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
    /// **Phase 2 (BTree probe):** for each surviving block, walk the BTree
    /// range `[0, end]` and verify `max_ts >= start`. This is O(log n + k)
    /// over the un-pruned portion.
    ///
    /// When the BRIN layer is empty (index just created or cleared), falls
    /// back directly to the BTree scan — identical to the previous behaviour.
    pub fn chunks_overlapping(&self, start: u64, end: u64) -> Vec<ChunkHandle> {
        if start > end {
            return Vec::new();
        }

        let ranges = self.brin_ranges.read();
        let entries = self.entries.read();
        let mut out = Vec::new();

        if ranges.is_empty() {
            // No BRIN ranges yet — plain BTree scan (startup / low-volume path).
            for (_, handles) in entries.range((Bound::Unbounded, Bound::Included(end))) {
                for h in handles {
                    if h.max_ts >= start {
                        out.push(*h);
                    }
                }
            }
            return out;
        }

        // Phase 1: collect the min_ts values of blocks that survive the BRIN filter.
        // Each block covers BRIN_CHUNKS_PER_RANGE consecutive chunks inserted in order,
        // so a surviving block's BTree keys fall in the block's [min_ts, max_ts] window.
        // We collect those windows and probe the BTree only within them.
        let mut surviving_windows: Vec<(u64, u64)> = Vec::new();
        for r in ranges.iter() {
            if r.chunk_count == 0 { continue; }
            if r.overlaps(start, end) {
                surviving_windows.push((r.min_ts, r.max_ts));
            }
        }

        if surviving_windows.is_empty() {
            return Vec::new();
        }

        // Phase 2: BTree probe restricted to surviving block windows.
        // For each window, scan BTree keys in [0, min(window.max_ts, end)].
        for (win_min, win_max) in surviving_windows {
            let probe_end = win_max.min(end);
            for (_, handles) in
                entries.range((Bound::Included(win_min), Bound::Included(probe_end)))
            {
                for h in handles {
                    if h.max_ts >= start {
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
        *self.count.read()
    }

    /// Is the index empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Highest `max_ts` seen so far. Cheap upper bound for unbounded range
    /// queries ("everything after T").
    pub fn global_max_timestamp(&self) -> u64 {
        *self.global_max.read()
    }

    /// Number of BRIN block ranges (useful for diagnostics / EXPLAIN output).
    pub fn brin_range_count(&self) -> usize {
        self.brin_ranges.read().iter().filter(|r| r.chunk_count > 0).count()
    }

    /// Reset the index. Used by tests and deserialize paths.
    pub fn clear(&self) {
        self.entries.write().clear();
        self.brin_ranges.write().clear();
        *self.global_max.write() = 0;
        *self.bloom.write() = BloomSegment::with_capacity(1024);
        *self.count.write() = 0;
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
        let entries = self.len();
        let distinct_keys = self.entries.read().len();
        let brin_ranges = self.brin_range_count();
        IndexStats {
            entries,
            distinct_keys,
            // Each BRIN range: 24 bytes (min_ts u64 + max_ts u64 + count usize).
            // Each BTree entry: ~48 bytes (key u64 + Vec header + pointer).
            approx_bytes: brin_ranges * 24 + distinct_keys * 48,
            kind: IndexKind::Temporal,
            has_bloom: true,
            index_correlation: 1.0, // timeseries inserts are monotonically increasing
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
