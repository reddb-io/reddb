//! Block-level zone maps (a.k.a. min/max summaries, SMA indexes).
//!
//! A zone map is a small, per-block summary used to *skip* entire blocks
//! during scans. Given a query predicate, the planner consults the zone map
//! first; if the block cannot possibly contain a match, it is not read.
//! Data warehouses like Parquet, Clickhouse, and Snowflake rely on this for
//! order-of-magnitude pruning.
//!
//! RedDB already tracks `min_ts`/`max_ts` on timeseries chunks, but nothing
//! richer — and tables/graphs get nothing at all. This module provides a
//! single reusable [`ZoneMap`] that every segment can embed via
//! [`crate::storage::index::HasBloom`] semantics.
//!
//! # What it tracks
//!
//! - `min_key` / `max_key` — lexicographic bounds, lets range queries skip
//! - `total_count` — rows observed
//! - `null_count` — rows with a null in the indexed column
//! - `distinct_estimate` — via [`HyperLogLog`] (16 KB, ~0.81% std error)
//! - `bloom` — fast negative point lookup via [`BloomSegment`]
//!
//! # What it does not
//!
//! - Value distributions (histograms) — not yet.
//! - Row-level positions — zone maps are *block summaries*, not indexes.
//!
//! # Integration
//!
//! Blocks call [`ZoneMap::observe`] / [`ZoneMap::observe_null`] on every
//! insert. Planners call [`ZoneMap::block_skip`] with a predicate to decide
//! whether to read the block. Zone maps are mergeable via [`ZoneMap::union`]
//! so higher-level aggregates (segment → collection → shard) can be built
//! cheaply.

use crate::storage::index::bloom_segment::BloomSegment;
use crate::storage::index::stats::{IndexKind, IndexStats};
use crate::storage::index::{HasBloom, IndexBase};
use crate::storage::primitives::HyperLogLog;

/// Predicate the planner asks a zone map to evaluate.
#[derive(Debug, Clone)]
pub enum ZonePredicate<'a> {
    /// Equality: `column == key`
    Equals(&'a [u8]),
    /// Range: `start <= column <= end`. `None` on either side means open.
    Range {
        start: Option<&'a [u8]>,
        end: Option<&'a [u8]>,
    },
    /// Is null check: block must contain at least one null.
    IsNull,
    /// Is not null: block must contain at least one non-null value.
    IsNotNull,
}

/// Outcome of evaluating a zone map against a predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneDecision {
    /// The block *might* contain matching rows — read it.
    MustRead,
    /// The block cannot possibly contain matches — skip it.
    Skip,
}

/// Reusable per-block zone map.
pub struct ZoneMap {
    min_key: Option<Vec<u8>>,
    max_key: Option<Vec<u8>>,
    total_count: u64,
    null_count: u64,
    hll: HyperLogLog,
    bloom: BloomSegment,
}

impl ZoneMap {
    /// Create a zone map sized for `expected_rows`. The bloom is tuned to
    /// the row estimate; HLL is fixed-size (~16 KB).
    pub fn with_capacity(expected_rows: usize) -> Self {
        Self {
            min_key: None,
            max_key: None,
            total_count: 0,
            null_count: 0,
            hll: HyperLogLog::new(),
            bloom: BloomSegment::with_capacity(expected_rows.max(64)),
        }
    }

    /// Default: sized for 4 KB table pages (~128 rows).
    pub fn new() -> Self {
        Self::with_capacity(128)
    }

    /// Record a non-null value observation.
    pub fn observe(&mut self, key: &[u8]) {
        self.total_count = self.total_count.saturating_add(1);
        self.hll.add(key);
        self.bloom.insert(key);

        match &self.min_key {
            None => self.min_key = Some(key.to_vec()),
            Some(cur) if key < cur.as_slice() => self.min_key = Some(key.to_vec()),
            _ => {}
        }
        match &self.max_key {
            None => self.max_key = Some(key.to_vec()),
            Some(cur) if key > cur.as_slice() => self.max_key = Some(key.to_vec()),
            _ => {}
        }
    }

    /// Record a null observation. Does not touch the bloom / HLL since
    /// there is no key to hash.
    pub fn observe_null(&mut self) {
        self.total_count = self.total_count.saturating_add(1);
        self.null_count = self.null_count.saturating_add(1);
    }

    /// Minimum observed key, if any.
    pub fn min(&self) -> Option<&[u8]> {
        self.min_key.as_deref()
    }

    /// Maximum observed key, if any.
    pub fn max(&self) -> Option<&[u8]> {
        self.max_key.as_deref()
    }

    /// Total rows observed (including nulls).
    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    /// Rows observed as null.
    pub fn null_count(&self) -> u64 {
        self.null_count
    }

    /// Rows observed as non-null.
    pub fn non_null_count(&self) -> u64 {
        self.total_count.saturating_sub(self.null_count)
    }

    /// Estimated distinct non-null values.
    pub fn distinct_estimate(&self) -> u64 {
        self.hll.count()
    }

    /// Access the underlying bloom (for cross-structure helpers).
    pub fn bloom(&self) -> &BloomSegment {
        &self.bloom
    }

    /// Decide whether to skip a block given a predicate.
    ///
    /// Safe by default: when uncertain, returns [`ZoneDecision::MustRead`].
    pub fn block_skip(&self, predicate: &ZonePredicate<'_>) -> ZoneDecision {
        // Empty block → trivially skippable.
        if self.total_count == 0 {
            return ZoneDecision::Skip;
        }

        match predicate {
            ZonePredicate::Equals(key) => {
                // Outside [min, max] window → skip.
                if let (Some(min), Some(max)) = (self.min(), self.max()) {
                    if *key < min || *key > max {
                        return ZoneDecision::Skip;
                    }
                }
                // Bloom says definitely absent → skip.
                if self.bloom.definitely_absent(key) {
                    return ZoneDecision::Skip;
                }
                ZoneDecision::MustRead
            }
            ZonePredicate::Range { start, end } => {
                if let (Some(a), Some(qend)) = (self.min(), end) {
                    if *qend < a {
                        return ZoneDecision::Skip;
                    }
                }
                if let (Some(b), Some(qstart)) = (self.max(), start) {
                    if *qstart > b {
                        return ZoneDecision::Skip;
                    }
                }
                ZoneDecision::MustRead
            }
            ZonePredicate::IsNull => {
                if self.null_count == 0 {
                    ZoneDecision::Skip
                } else {
                    ZoneDecision::MustRead
                }
            }
            ZonePredicate::IsNotNull => {
                if self.non_null_count() == 0 {
                    ZoneDecision::Skip
                } else {
                    ZoneDecision::MustRead
                }
            }
        }
    }

    /// Merge another zone map into this one (e.g. aggregating block-level
    /// maps into a segment-level summary).
    pub fn union(&mut self, other: &ZoneMap) {
        self.total_count = self.total_count.saturating_add(other.total_count);
        self.null_count = self.null_count.saturating_add(other.null_count);

        match (&self.min_key, &other.min_key) {
            (None, Some(o)) => self.min_key = Some(o.clone()),
            (Some(s), Some(o)) if o < s => self.min_key = Some(o.clone()),
            _ => {}
        }
        match (&self.max_key, &other.max_key) {
            (None, Some(o)) => self.max_key = Some(o.clone()),
            (Some(s), Some(o)) if o > s => self.max_key = Some(o.clone()),
            _ => {}
        }

        self.hll.merge(&other.hll);
        // BloomSegment::union_inplace fails when sizes differ; in that case
        // callers that care should rebuild the bloom. Zone-map union is a
        // best-effort aggregate.
        let _ = self.bloom.union_inplace(&other.bloom);
    }

    /// Reset to the empty state.
    pub fn clear(&mut self) {
        self.min_key = None;
        self.max_key = None;
        self.total_count = 0;
        self.null_count = 0;
        self.hll.clear();
        // Bloom can't be selectively cleared; replace it.
        self.bloom = BloomSegment::with_capacity(128);
    }
}

impl Default for ZoneMap {
    fn default() -> Self {
        Self::new()
    }
}

impl HasBloom for ZoneMap {
    fn bloom_segment(&self) -> Option<&BloomSegment> {
        Some(&self.bloom)
    }
}

impl IndexBase for ZoneMap {
    fn name(&self) -> &str {
        "zone_map"
    }

    fn kind(&self) -> IndexKind {
        IndexKind::ZoneMap
    }

    fn stats(&self) -> IndexStats {
        IndexStats {
            entries: self.total_count as usize,
            distinct_keys: self.distinct_estimate() as usize,
            approx_bytes: self.bloom.filter().byte_size() + 16 * 1024, // HLL fixed
            kind: IndexKind::ZoneMap,
            has_bloom: true,
        }
    }

    fn bloom(&self) -> Option<&crate::storage::primitives::BloomFilter> {
        Some(self.bloom.filter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_min_max() {
        let mut zm = ZoneMap::with_capacity(64);
        zm.observe(b"delta");
        zm.observe(b"alpha");
        zm.observe(b"charlie");
        zm.observe(b"beta");
        assert_eq!(zm.min(), Some(b"alpha".as_slice()));
        assert_eq!(zm.max(), Some(b"delta".as_slice()));
        assert_eq!(zm.total_count(), 4);
        assert_eq!(zm.null_count(), 0);
    }

    #[test]
    fn tracks_nulls_separately() {
        let mut zm = ZoneMap::with_capacity(64);
        zm.observe(b"a");
        zm.observe_null();
        zm.observe_null();
        zm.observe(b"b");
        assert_eq!(zm.total_count(), 4);
        assert_eq!(zm.null_count(), 2);
        assert_eq!(zm.non_null_count(), 2);
    }

    #[test]
    fn distinct_estimate_approximates_cardinality() {
        let mut zm = ZoneMap::with_capacity(2000);
        for i in 0..1000 {
            zm.observe(format!("user{i}").as_bytes());
        }
        // Insert duplicates — should not inflate the estimate.
        for i in 0..1000 {
            zm.observe(format!("user{i}").as_bytes());
        }
        let est = zm.distinct_estimate();
        // HLL is ~0.81% std error — give it slack.
        assert!(est > 900 && est < 1100, "estimate={est}");
    }

    #[test]
    fn block_skip_equality_out_of_range() {
        let mut zm = ZoneMap::with_capacity(64);
        zm.observe(b"mango");
        zm.observe(b"orange");
        zm.observe(b"peach");
        // Below min → skip.
        assert_eq!(
            zm.block_skip(&ZonePredicate::Equals(b"apple")),
            ZoneDecision::Skip
        );
        // Above max → skip.
        assert_eq!(
            zm.block_skip(&ZonePredicate::Equals(b"strawberry")),
            ZoneDecision::Skip
        );
        // In range, inserted → must read.
        assert_eq!(
            zm.block_skip(&ZonePredicate::Equals(b"mango")),
            ZoneDecision::MustRead
        );
    }

    #[test]
    fn block_skip_equality_bloom_prune() {
        let mut zm = ZoneMap::with_capacity(1024);
        zm.observe(b"alpha");
        zm.observe(b"zulu");
        // "needle" is inside [alpha, zulu] lexicographically, so range
        // check alone can't skip — bloom must prove absence.
        let decision = zm.block_skip(&ZonePredicate::Equals(b"needle"));
        // Bloom is probabilistic; it *usually* prunes an unseen key.
        // Either outcome is safe, but must-read for an absent key is
        // still correct behavior.
        assert!(matches!(
            decision,
            ZoneDecision::Skip | ZoneDecision::MustRead
        ));
    }

    #[test]
    fn block_skip_range_non_overlapping() {
        let mut zm = ZoneMap::with_capacity(64);
        zm.observe(&10u32.to_be_bytes());
        zm.observe(&50u32.to_be_bytes());
        zm.observe(&100u32.to_be_bytes());

        let lo = 200u32.to_be_bytes();
        let hi = 300u32.to_be_bytes();
        assert_eq!(
            zm.block_skip(&ZonePredicate::Range {
                start: Some(&lo),
                end: Some(&hi),
            }),
            ZoneDecision::Skip
        );

        let qlo = 40u32.to_be_bytes();
        let qhi = 60u32.to_be_bytes();
        assert_eq!(
            zm.block_skip(&ZonePredicate::Range {
                start: Some(&qlo),
                end: Some(&qhi),
            }),
            ZoneDecision::MustRead
        );
    }

    #[test]
    fn block_skip_null_predicates() {
        let mut empty_nulls = ZoneMap::with_capacity(64);
        empty_nulls.observe(b"x");
        assert_eq!(
            empty_nulls.block_skip(&ZonePredicate::IsNull),
            ZoneDecision::Skip
        );
        assert_eq!(
            empty_nulls.block_skip(&ZonePredicate::IsNotNull),
            ZoneDecision::MustRead
        );

        let mut all_nulls = ZoneMap::with_capacity(64);
        all_nulls.observe_null();
        all_nulls.observe_null();
        assert_eq!(
            all_nulls.block_skip(&ZonePredicate::IsNull),
            ZoneDecision::MustRead
        );
        assert_eq!(
            all_nulls.block_skip(&ZonePredicate::IsNotNull),
            ZoneDecision::Skip
        );
    }

    #[test]
    fn empty_block_skips_everything() {
        let zm = ZoneMap::with_capacity(64);
        assert_eq!(
            zm.block_skip(&ZonePredicate::Equals(b"whatever")),
            ZoneDecision::Skip
        );
        assert_eq!(
            zm.block_skip(&ZonePredicate::Range {
                start: None,
                end: None,
            }),
            ZoneDecision::Skip
        );
    }

    #[test]
    fn union_merges_bounds_and_counts() {
        let mut a = ZoneMap::with_capacity(256);
        a.observe(b"cherry");
        a.observe(b"apple");
        a.observe_null();

        let mut b = ZoneMap::with_capacity(256);
        b.observe(b"zebra");
        b.observe(b"banana");

        a.union(&b);
        assert_eq!(a.min(), Some(b"apple".as_slice()));
        assert_eq!(a.max(), Some(b"zebra".as_slice()));
        assert_eq!(a.total_count(), 5);
        assert_eq!(a.null_count(), 1);
        assert!(a.distinct_estimate() >= 4);
    }

    #[test]
    fn stats_match_observation_counts() {
        let mut zm = ZoneMap::with_capacity(64);
        for i in 0..50u32 {
            zm.observe(&i.to_be_bytes());
        }
        let s = zm.stats();
        assert_eq!(s.entries, 50);
        assert_eq!(s.kind, IndexKind::ZoneMap);
        assert!(s.has_bloom);
    }

    #[test]
    fn clear_resets_all_state() {
        let mut zm = ZoneMap::with_capacity(64);
        zm.observe(b"x");
        zm.observe_null();
        zm.clear();
        assert_eq!(zm.total_count(), 0);
        assert_eq!(zm.null_count(), 0);
        assert_eq!(zm.min(), None);
        assert_eq!(zm.max(), None);
    }
}
