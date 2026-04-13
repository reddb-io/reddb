//! Index statistics and kind enumeration surfaced by [`super::IndexBase`].
//!
//! Separated from the traits so the planner and diagnostics layers can depend
//! on the types without pulling in the generic trait definitions.

/// Enumeration of all index families RedDB understands. New structures add a
/// variant here so the planner can match on them and pick cost models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IndexKind {
    /// Placeholder used by `IndexStats::default()`.
    #[default]
    Unknown,
    /// Ordered B-tree / B+ tree (range + point).
    BTree,
    /// Hash index (point-only, O(1)).
    Hash,
    /// Bitmap index (low-cardinality, set operations).
    Bitmap,
    /// Roaring bitmap compressed variant.
    RoaringBitmap,
    /// Inverted index for full-text / tokenised lookups.
    Inverted,
    /// HNSW approximate nearest-neighbour.
    Hnsw,
    /// IVF-Flat vector index.
    IvfFlat,
    /// Product-quantised vector index.
    ProductQuantization,
    /// Spatial index (R-tree / geo-hash).
    Spatial,
    /// Adjacency index for graph edges.
    GraphAdjacency,
    /// Temporal BTree for timeseries.
    Temporal,
    /// Cuckoo filter.
    Cuckoo,
    /// Zone map / min-max summary.
    ZoneMap,
    /// Cross-structure unified reference index.
    UnifiedRef,
    /// Count-min sketch heavy-hitters (top-k frequency estimate).
    HeavyHitters,
}

impl IndexKind {
    /// Does this kind support range queries out of the box?
    pub fn supports_range(self) -> bool {
        matches!(
            self,
            IndexKind::BTree | IndexKind::Spatial | IndexKind::Temporal | IndexKind::ZoneMap
        )
    }

    /// Does this kind support approximate / similarity queries?
    pub fn supports_ann(self) -> bool {
        matches!(
            self,
            IndexKind::Hnsw | IndexKind::IvfFlat | IndexKind::ProductQuantization
        )
    }
}

/// Per-index statistics used by the cost-based planner and diagnostics.
/// All fields are best-effort; zero means "unknown".
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    /// Total number of `(key, value)` pairs stored.
    pub entries: usize,
    /// Number of distinct keys. For hash/btree this equals the key set size.
    pub distinct_keys: usize,
    /// Approximate memory footprint in bytes (0 if not tracked).
    pub approx_bytes: usize,
    /// Family of index this originates from.
    pub kind: IndexKind,
    /// Whether a bloom filter is attached (enables fast negative lookups).
    pub has_bloom: bool,
}

impl IndexStats {
    /// Rough selectivity estimate for an equality predicate. Returns the
    /// expected fraction of rows matching a random key, clamped to `[0, 1]`.
    ///
    /// Used by the planner to pick between index probes and full scans.
    pub fn point_selectivity(&self) -> f64 {
        if self.distinct_keys == 0 {
            return 1.0;
        }
        (1.0 / self.distinct_keys as f64).clamp(0.0, 1.0)
    }

    /// Average number of values per distinct key.
    pub fn avg_values_per_key(&self) -> f64 {
        if self.distinct_keys == 0 {
            return 0.0;
        }
        self.entries as f64 / self.distinct_keys as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectivity_scales_with_cardinality() {
        let s = IndexStats {
            entries: 100,
            distinct_keys: 100,
            approx_bytes: 0,
            kind: IndexKind::Hash,
            has_bloom: false,
        };
        assert_eq!(s.point_selectivity(), 0.01);
        assert_eq!(s.avg_values_per_key(), 1.0);
    }

    #[test]
    fn range_capability_flag() {
        assert!(IndexKind::BTree.supports_range());
        assert!(!IndexKind::Hash.supports_range());
        assert!(IndexKind::Hnsw.supports_ann());
    }

    #[test]
    fn empty_stats_do_not_divide_by_zero() {
        let s = IndexStats::default();
        assert_eq!(s.point_selectivity(), 1.0);
        assert_eq!(s.avg_values_per_key(), 0.0);
    }
}
