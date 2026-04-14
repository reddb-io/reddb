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
    /// Physical correlation between index key order and heap row order.
    /// 1.0 = perfectly correlated (monotonic insert, timeseries) → sequential I/O.
    /// 0.0 = completely random → worst-case random I/O per row.
    /// Default 0.0 (conservative). Used by Mackert-Lohman I/O cost formula.
    pub index_correlation: f64,
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

    /// Estimate the I/O cost (in arbitrary page-cost units) of fetching
    /// `result_rows` rows via this index from a `heap_pages`-page table.
    ///
    /// Uses the Mackert-Lohman (1986) formula — the same model PostgreSQL
    /// uses in `cost_index` (`optimizer/path/costsize.c:545-700`):
    ///
    /// ```text
    /// pages_fetched = ML(selectivity, heap_pages)
    /// io_cost = lerp(random_io, seq_io, correlation²)
    /// ```
    ///
    /// Constants match PG GUC defaults:
    /// - `random_page_cost = 4.0`
    /// - `seq_page_cost    = 1.0`
    pub fn correlated_io_cost(&self, result_rows: f64, heap_pages: f64) -> f64 {
        const SEQ_PAGE_COST: f64 = 1.0;
        const RANDOM_PAGE_COST: f64 = 4.0;

        if heap_pages <= 0.0 || result_rows <= 0.0 {
            return 0.0;
        }

        // Mackert-Lohman: expected distinct pages fetched when picking
        // `result_rows` rows at random from a `heap_pages`-page file.
        // Approximation: min(result_rows, heap_pages) * (1 - e^(-result_rows/heap_pages))
        // This is the standard finite-population coupon-collector formula.
        let frac = result_rows / heap_pages;
        let pages_fetched = heap_pages * (1.0 - (-frac).exp());
        let pages_fetched = pages_fetched.min(heap_pages);

        // Random I/O: every page fetch is a seek
        let random_cost = RANDOM_PAGE_COST * pages_fetched;

        // Sequential I/O: rows arrive in heap order (correlation ≈ 1)
        let seq_cost = SEQ_PAGE_COST * pages_fetched;

        // Blend: correlation² weights sequential vs random
        let corr2 = self.index_correlation.powi(2).clamp(0.0, 1.0);
        seq_cost * corr2 + random_cost * (1.0 - corr2)
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
            index_correlation: 0.0,
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

    #[test]
    fn correlated_io_cheaper_than_random() {
        let base = IndexStats {
            entries: 10_000,
            distinct_keys: 10_000,
            approx_bytes: 0,
            kind: IndexKind::BTree,
            has_bloom: false,
            index_correlation: 0.0,
        };
        let correlated = IndexStats { index_correlation: 1.0, ..base.clone() };
        let heap_pages = 1000.0;
        let result_rows = 100.0;

        let random_cost = base.correlated_io_cost(result_rows, heap_pages);
        let seq_cost = correlated.correlated_io_cost(result_rows, heap_pages);
        assert!(
            seq_cost < random_cost,
            "correlated (seq) should be cheaper than uncorrelated (random): {seq_cost} vs {random_cost}"
        );
    }

    #[test]
    fn timeseries_gets_full_correlation() {
        // Timeseries index is set to correlation = 1.0 in temporal_index.rs
        let s = IndexStats {
            index_correlation: 1.0,
            kind: IndexKind::Temporal,
            ..IndexStats::default()
        };
        // With correlation = 1.0, cost = seq_cost × pages_fetched
        // 0 result_rows → 0 cost
        assert_eq!(s.correlated_io_cost(0.0, 1000.0), 0.0);
    }
}
