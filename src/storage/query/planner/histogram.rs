//! Equi-depth histograms and most-common-value lists for the planner.
//!
//! Mirrors PostgreSQL's `pg_statistic` histogram + MCV machinery
//! (`src/backend/utils/adt/selfuncs.c::histogram_selectivity` and
//! `var_eq_const`). Lets the cost estimator replace the uniform
//! `0.3 / 0.01` heuristics with bucket-arithmetic for ranges and
//! frequency-lookup for equality on skewed columns.
//!
//! The data structures are pure Rust and column-type-agnostic: a small
//! [`ColumnValue`] enum represents the comparable subset of values
//! (`Int`, `Float`, `Text`) — enough for the columns the planner cares
//! about, with room to extend later. The `equi_depth_from_sample`
//! builder constructs histograms from a pre-sampled `Vec<ColumnValue>`
//! by sorting and partitioning.
//!
//! Both structures are **opt-in** through the `StatsProvider` trait
//! (default returns `None`), so adding histograms is strictly additive
//! — call sites that don't supply one keep using the existing
//! heuristic path with no surprises.
//!
//! See `src/storage/query/planner/README.md` § Invariant 4.

use std::cmp::Ordering;

/// Comparable column value used by histogram bucket arithmetic and MCV
/// frequency lookup.
///
/// Intentionally narrow: real reddb columns can be any AST `Value`,
/// but only a few types are useful for selectivity estimation. Other
/// values simply don't get histograms — `filter_selectivity` falls
/// back to the heuristic path.
#[derive(Debug, Clone)]
pub enum ColumnValue {
    Int(i64),
    Float(f64),
    Text(String),
}

impl ColumnValue {
    fn cmp_inner(&self, other: &ColumnValue) -> Ordering {
        match (self, other) {
            (ColumnValue::Int(a), ColumnValue::Int(b)) => a.cmp(b),
            (ColumnValue::Float(a), ColumnValue::Float(b)) => {
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (ColumnValue::Text(a), ColumnValue::Text(b)) => a.cmp(b),
            // Cross-type comparisons fall back to a stable but
            // semantically meaningless order (Int < Float < Text). The
            // planner never mixes types in a single histogram so this
            // is safe in practice.
            (ColumnValue::Int(_), _) => Ordering::Less,
            (ColumnValue::Float(_), ColumnValue::Int(_)) => Ordering::Greater,
            (ColumnValue::Float(_), _) => Ordering::Less,
            (ColumnValue::Text(_), _) => Ordering::Greater,
        }
    }
}

impl PartialEq for ColumnValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_inner(other) == Ordering::Equal
    }
}

impl Eq for ColumnValue {}

impl std::hash::Hash for ColumnValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            ColumnValue::Int(v) => v.hash(state),
            ColumnValue::Float(v) => v.to_bits().hash(state),
            ColumnValue::Text(v) => v.hash(state),
        }
    }
}

impl PartialOrd for ColumnValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp_inner(other))
    }
}

impl Ord for ColumnValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_inner(other)
    }
}

/// One equi-depth bucket: every bucket holds *roughly* the same
/// number of rows, so bucket count along the `min..max` interval
/// estimates the row count in any range query subset.
#[derive(Debug, Clone)]
pub struct Bucket {
    pub min: ColumnValue,
    pub max: ColumnValue,
    pub count: u64,
}

/// Equi-depth histogram over a single column.
///
/// Buckets are non-overlapping and sorted by `min`. Each bucket holds
/// roughly `total_count / bucket_count` rows. Range selectivity is
/// computed by counting buckets fully inside the query interval and
/// approximating the partial buckets at the edges.
#[derive(Debug, Clone)]
pub struct Histogram {
    pub buckets: Vec<Bucket>,
    pub total_count: u64,
}

impl Histogram {
    /// Build an equi-depth histogram from an in-memory sample.
    ///
    /// `bucket_count` is clamped between 1 and `values.len()`. The
    /// caller is responsible for sampling — passing the full table
    /// works but is expensive on large columns.
    pub fn equi_depth_from_sample(mut values: Vec<ColumnValue>, bucket_count: usize) -> Self {
        if values.is_empty() {
            return Self {
                buckets: Vec::new(),
                total_count: 0,
            };
        }
        let bucket_count = bucket_count.clamp(1, values.len());
        values.sort();
        let total = values.len();
        let per_bucket = total / bucket_count;
        let mut remainder = total % bucket_count;
        let mut buckets = Vec::with_capacity(bucket_count);
        let mut idx = 0;
        for _ in 0..bucket_count {
            let take = if remainder > 0 {
                remainder -= 1;
                per_bucket + 1
            } else {
                per_bucket
            };
            if take == 0 {
                break;
            }
            let end = (idx + take).min(values.len());
            let min = values[idx].clone();
            let max = values[end - 1].clone();
            buckets.push(Bucket {
                min,
                max,
                count: take as u64,
            });
            idx = end;
        }
        Self {
            buckets,
            total_count: total as u64,
        }
    }

    /// Estimate the fraction of rows whose value falls in
    /// `[lo, hi]` (both bounds inclusive when present).
    ///
    /// `None` for either bound means "open" — `[lo..]` for upper-open,
    /// `[..hi]` for lower-open, and `None`/`None` returns `1.0`
    /// (every row qualifies).
    ///
    /// Returns a value clamped to `[0.0, 1.0]`.
    pub fn range_selectivity(&self, lo: Option<&ColumnValue>, hi: Option<&ColumnValue>) -> f64 {
        if self.total_count == 0 || self.buckets.is_empty() {
            return 1.0;
        }
        let mut matched: f64 = 0.0;
        for bucket in &self.buckets {
            let bucket_size = bucket.count as f64;
            // Determine the overlap of [bucket.min, bucket.max] with
            // [lo, hi]. Both intervals are inclusive on each side.
            let bucket_below_query = hi.is_some() && bucket.min > *hi.unwrap();
            let bucket_above_query = lo.is_some() && bucket.max < *lo.unwrap();
            if bucket_below_query || bucket_above_query {
                continue;
            }
            // Bucket fully inside the query?
            let fully_inside_low = lo.is_none() || bucket.min >= *lo.unwrap();
            let fully_inside_high = hi.is_none() || bucket.max <= *hi.unwrap();
            if fully_inside_low && fully_inside_high {
                matched += bucket_size;
                continue;
            }
            // Partial overlap. Use a simple linear-interpolation fraction
            // assuming uniform distribution within the bucket. We
            // approximate the fraction as 0.5 since reddb columns can
            // be non-numeric (text), where linear interpolation isn't
            // defined. This matches postgres' fallback for non-numeric
            // histograms.
            matched += bucket_size * 0.5;
        }
        let result = matched / self.total_count as f64;
        result.clamp(0.0, 1.0)
    }

    /// Number of buckets in the histogram.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

/// Most-common-values list for a column.
///
/// Each entry is `(value, frequency)` where frequency is in `[0, 1]`
/// — the fraction of total rows holding that exact value. Use for
/// equality selectivity on skewed columns where one or two values
/// dominate.
#[derive(Debug, Clone, Default)]
pub struct MostCommonValues {
    pub values: Vec<(ColumnValue, f64)>,
}

impl MostCommonValues {
    /// Construct from a `(value, frequency)` slice. Frequencies are
    /// expected to already be in `[0, 1]`; the constructor sorts by
    /// frequency descending so callers can `.values[0]` to get the
    /// hottest key.
    pub fn new(mut entries: Vec<(ColumnValue, f64)>) -> Self {
        entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        Self { values: entries }
    }

    /// Lookup `value` in the MCV list and return its frequency, or
    /// `None` if it isn't tracked.
    pub fn frequency_of(&self, value: &ColumnValue) -> Option<f64> {
        self.values
            .iter()
            .find(|(v, _)| v == value)
            .map(|(_, f)| *f)
    }

    /// Sum of all tracked frequencies (always `<= 1.0` if constructed
    /// from a valid sample).
    pub fn total_frequency(&self) -> f64 {
        self.values.iter().map(|(_, f)| *f).sum()
    }

    /// Number of MCVs tracked.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the MCV list is empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(vals: &[i64]) -> Vec<ColumnValue> {
        vals.iter().map(|&i| ColumnValue::Int(i)).collect()
    }

    #[test]
    fn empty_sample_produces_empty_histogram() {
        let h = Histogram::equi_depth_from_sample(vec![], 4);
        assert_eq!(h.bucket_count(), 0);
        assert_eq!(h.total_count, 0);
        // Empty histogram is treated as "no information" → returns 1.0.
        assert_eq!(h.range_selectivity(None, None), 1.0);
    }

    #[test]
    fn equi_depth_buckets_are_roughly_equal_count() {
        let sample: Vec<ColumnValue> = (0..100i64).map(ColumnValue::Int).collect();
        let h = Histogram::equi_depth_from_sample(sample, 10);
        assert_eq!(h.bucket_count(), 10);
        assert_eq!(h.total_count, 100);
        for bucket in &h.buckets {
            assert_eq!(bucket.count, 10);
        }
    }

    #[test]
    fn equi_depth_distributes_remainder() {
        // 13 values into 4 buckets → some buckets get an extra.
        let sample: Vec<ColumnValue> = (0..13i64).map(ColumnValue::Int).collect();
        let h = Histogram::equi_depth_from_sample(sample, 4);
        assert_eq!(h.bucket_count(), 4);
        let total: u64 = h.buckets.iter().map(|b| b.count).sum();
        assert_eq!(total, 13);
        // First buckets get the remainder (4, 4, 4, ... wait 13/4 = 3 r 1)
        // → first bucket gets 4, rest get 3.
        let counts: Vec<u64> = h.buckets.iter().map(|b| b.count).collect();
        assert_eq!(counts.iter().sum::<u64>(), 13);
        assert!(counts.iter().all(|&c| c >= 3 && c <= 4));
    }

    #[test]
    fn range_selectivity_full_table_is_one() {
        let h = Histogram::equi_depth_from_sample(ints(&[1, 2, 3, 4, 5]), 5);
        // No bounds → entire table.
        assert_eq!(h.range_selectivity(None, None), 1.0);
    }

    #[test]
    fn range_selectivity_below_min_is_zero() {
        let h = Histogram::equi_depth_from_sample(ints(&[10, 20, 30, 40]), 4);
        let zero = ColumnValue::Int(0);
        let five = ColumnValue::Int(5);
        let s = h.range_selectivity(Some(&zero), Some(&five));
        assert_eq!(s, 0.0);
    }

    #[test]
    fn range_selectivity_above_max_is_zero() {
        let h = Histogram::equi_depth_from_sample(ints(&[10, 20, 30, 40]), 4);
        let hi = ColumnValue::Int(100);
        let lo = ColumnValue::Int(50);
        let s = h.range_selectivity(Some(&lo), Some(&hi));
        assert_eq!(s, 0.0);
    }

    #[test]
    fn histogram_beats_uniform_on_skewed_range() {
        // 80% of values in [0, 9], 20% in [10, 1000].
        let mut sample: Vec<ColumnValue> = Vec::new();
        for i in 0..80 {
            sample.push(ColumnValue::Int(i % 10));
        }
        for i in 0..20 {
            sample.push(ColumnValue::Int(10 + i * 50));
        }
        let h = Histogram::equi_depth_from_sample(sample, 10);
        // Query: value <= 9. Histogram should report ~0.8 (the heavy
        // half), which is far better than the heuristic 0.3.
        let nine = ColumnValue::Int(9);
        let s = h.range_selectivity(None, Some(&nine));
        assert!(s > 0.5, "histogram selectivity {} should exceed 0.5", s);
        assert!(s <= 1.0);
    }

    #[test]
    fn range_selectivity_clamped_to_unit_interval() {
        let h = Histogram::equi_depth_from_sample(ints(&[1, 2, 3]), 3);
        // Spurious bounds: completely unmatchable.
        let lo = ColumnValue::Int(99);
        let hi = ColumnValue::Int(100);
        let s = h.range_selectivity(Some(&lo), Some(&hi));
        assert!((0.0..=1.0).contains(&s));
        assert_eq!(s, 0.0);
    }

    // ---------- MostCommonValues -----------------------------------

    #[test]
    fn mcv_frequency_lookup() {
        let mcv = MostCommonValues::new(vec![
            (ColumnValue::Int(7), 0.5),
            (ColumnValue::Int(42), 0.3),
            (ColumnValue::Int(99), 0.05),
        ]);
        assert_eq!(mcv.frequency_of(&ColumnValue::Int(7)), Some(0.5));
        assert_eq!(mcv.frequency_of(&ColumnValue::Int(42)), Some(0.3));
        assert!(mcv.frequency_of(&ColumnValue::Int(0)).is_none());
    }

    #[test]
    fn mcv_total_frequency_sums_correctly() {
        let mcv = MostCommonValues::new(vec![
            (ColumnValue::Int(1), 0.4),
            (ColumnValue::Int(2), 0.3),
            (ColumnValue::Int(3), 0.2),
        ]);
        let total = mcv.total_frequency();
        assert!((total - 0.9).abs() < 1e-9);
    }

    #[test]
    fn mcv_sorts_by_frequency_descending() {
        let mcv = MostCommonValues::new(vec![
            (ColumnValue::Int(1), 0.1),
            (ColumnValue::Int(2), 0.5),
            (ColumnValue::Int(3), 0.2),
        ]);
        assert_eq!(mcv.values[0].1, 0.5);
        assert_eq!(mcv.values[1].1, 0.2);
        assert_eq!(mcv.values[2].1, 0.1);
    }

    #[test]
    fn mcv_beats_uniform_on_skewed_eq() {
        // One value (the boss) takes 50% of the table.
        let mcv = MostCommonValues::new(vec![
            (ColumnValue::text("boss".to_string()), 0.5),
            (ColumnValue::text("alice".to_string()), 0.05),
            (ColumnValue::text("bob".to_string()), 0.05),
        ]);
        let boss = ColumnValue::text("boss".to_string());
        let freq = mcv.frequency_of(&boss).unwrap();
        // Equality on the boss row: 0.5, vs heuristic 0.01 → 50× better.
        assert_eq!(freq, 0.5);
        assert!(freq > 0.01);
    }

    #[test]
    fn mcv_empty_when_no_values() {
        let mcv = MostCommonValues::new(vec![]);
        assert!(mcv.is_empty());
        assert_eq!(mcv.len(), 0);
        assert_eq!(mcv.total_frequency(), 0.0);
        assert!(mcv.frequency_of(&ColumnValue::Int(1)).is_none());
    }
}
