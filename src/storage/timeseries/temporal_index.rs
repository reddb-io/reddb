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
use std::sync::RwLock;

use crate::storage::index::{BloomSegment, HasBloom, IndexBase, IndexKind, IndexStats};

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

/// Temporal BTree index keyed by `min_ts`.
///
/// Multiple chunks may share the same `min_ts` so each key maps to a `Vec`.
pub struct TemporalIndex {
    entries: RwLock<BTreeMap<u64, Vec<ChunkHandle>>>,
    /// Global maximum `max_ts` across registered chunks. Used as the upper
    /// bound for unbounded range queries without walking to `u64::MAX`.
    global_max: RwLock<u64>,
    /// Bloom filter over registered `min_ts` values. Cheap negative check
    /// before touching the BTree.
    bloom: RwLock<BloomSegment>,
    /// Running count of registered handles. Tracked explicitly so stats
    /// don't have to walk the BTree.
    count: RwLock<usize>,
}

impl TemporalIndex {
    /// Create an empty index sized for `expected_chunks` entries.
    pub fn new(expected_chunks: usize) -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
            global_max: RwLock::new(0),
            bloom: RwLock::new(BloomSegment::with_capacity(expected_chunks.max(1024))),
            count: RwLock::new(0),
        }
    }

    /// Register a chunk handle. Safe to call from multiple threads — each
    /// registration takes the entries write lock briefly.
    pub fn register(&self, handle: ChunkHandle) {
        if let Ok(mut entries) = self.entries.write() {
            entries.entry(handle.min_ts).or_default().push(handle);
        }
        if let Ok(mut bloom) = self.bloom.write() {
            bloom.insert(&handle.min_ts.to_le_bytes());
        }
        if let Ok(mut m) = self.global_max.write() {
            if handle.max_ts > *m {
                *m = handle.max_ts;
            }
        }
        if let Ok(mut c) = self.count.write() {
            *c = c.saturating_add(1);
        }
    }

    /// Forget every handle with the given `chunk_id`. Does not touch the
    /// bloom (bloom filters don't support removal — stale positives cost
    /// at most an extra BTree probe that finds no match).
    pub fn unregister(&self, chunk_id: u64) -> usize {
        let mut removed = 0usize;
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|_, handles| {
                let before = handles.len();
                handles.retain(|h| h.chunk_id != chunk_id);
                removed += before - handles.len();
                !handles.is_empty()
            });
        }
        if removed > 0 {
            if let Ok(mut c) = self.count.write() {
                *c = c.saturating_sub(removed);
            }
        }
        removed
    }

    /// Return every handle whose interval overlaps `[start, end]`
    /// (both inclusive).
    ///
    /// Algorithm:
    /// 1. Walk the BTree from `0` up to `end` (keys greater than `end`
    ///    cannot start within the query window).
    /// 2. For each candidate, verify `max_ts >= start`.
    ///
    /// This is *not* optimal for very long-lived chunks (one 10-year chunk
    /// forces a full scan). An interval tree would be the next upgrade.
    pub fn chunks_overlapping(&self, start: u64, end: u64) -> Vec<ChunkHandle> {
        if start > end {
            return Vec::new();
        }
        let entries = match self.entries.read() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for (_, handles) in entries.range((Bound::Unbounded, Bound::Included(end))) {
            for h in handles {
                if h.max_ts >= start {
                    out.push(*h);
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
        self.bloom
            .read()
            .map(|b| b.contains(&ts.to_le_bytes()))
            .unwrap_or(true)
    }

    /// Number of registered chunks.
    pub fn len(&self) -> usize {
        self.count.read().map(|c| *c).unwrap_or(0)
    }

    /// Is the index empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Highest `max_ts` seen so far. Cheap upper bound for unbounded range
    /// queries ("everything after T").
    pub fn global_max_timestamp(&self) -> u64 {
        self.global_max.read().map(|m| *m).unwrap_or(0)
    }

    /// Reset the index. Used by tests and deserialize paths.
    pub fn clear(&self) {
        if let Ok(mut e) = self.entries.write() {
            e.clear();
        }
        if let Ok(mut m) = self.global_max.write() {
            *m = 0;
        }
        if let Ok(mut b) = self.bloom.write() {
            *b = BloomSegment::with_capacity(1024);
        }
        if let Ok(mut c) = self.count.write() {
            *c = 0;
        }
    }
}

impl Default for TemporalIndex {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl HasBloom for TemporalIndex {
    fn bloom_segment(&self) -> Option<&BloomSegment> {
        // RwLock precludes handing out a raw reference.
        None
    }

    fn definitely_absent(&self, key: &[u8]) -> bool {
        self.bloom
            .read()
            .map(|b| b.definitely_absent(key))
            .unwrap_or(false)
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
        let distinct_keys = self.entries.read().map(|e| e.len()).unwrap_or(0);
        IndexStats {
            entries,
            distinct_keys,
            approx_bytes: 0,
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
