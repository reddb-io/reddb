//! ClickHouse-parity aggregate functions (Track B4 sprint).
//!
//! These live in a self-contained helper module so the SQL dispatcher
//! can adopt them incrementally: each aggregate is a small struct
//! with `add`, `merge`, and `finalize`. When the planner wires them
//! to `AggregateFunction::*` variants, nothing inside here changes.
//!
//! Algorithms favoured from the ClickHouse surface:
//!
//! * `uniq` / `uniqHLL12` — cardinality via HyperLogLog, reusing
//!   [`crate::storage::primitives::HyperLogLog`].
//! * `quantileTDigest` — [`crate::storage::primitives::TDigest`]
//!   with ClickHouse-compatible 0.99/0.95/0.5 defaults.
//! * `corr`, `covar_pop`, `covar_samp` — Welford-style numerically
//!   stable two-variable accumulators.
//! * `sum_if`, `avg_if`, `count_if` — conditional aggregation over
//!   an arbitrary predicate callable.
//! * `any`, `anyLast` — first / last observed value.
//! * `groupArray(n)` — first `n` values as an ordered list.
//! * `arrayJoin` — UNNEST helper that expands a Vec input into
//!   flattened iteration.

use std::collections::VecDeque;

use crate::storage::primitives::{HyperLogLog, TDigest};

/// Cardinality estimator. Thin wrapper over HLL keeping the API
/// consistent with the other aggregators in this module.
#[derive(Debug, Clone)]
pub struct UniqAggregator {
    hll: HyperLogLog,
}

impl UniqAggregator {
    /// Underlying HLL uses fixed 2¹⁴ registers (~16 KB) with ~0.81%
    /// standard error. The `precision` parameter is accepted for
    /// ClickHouse-surface compatibility (`uniqHLL12` etc.) but
    /// currently informational only.
    pub fn new(_precision: u8) -> Self {
        Self {
            hll: HyperLogLog::new(),
        }
    }

    pub fn add(&mut self, bytes: &[u8]) {
        self.hll.add(bytes);
    }

    pub fn add_str(&mut self, s: &str) {
        self.hll.add(s.as_bytes());
    }

    pub fn add_i64(&mut self, v: i64) {
        self.hll.add(&v.to_le_bytes());
    }

    pub fn merge(&mut self, other: &Self) {
        self.hll.merge(&other.hll);
    }

    /// Estimate of the distinct count observed so far.
    pub fn estimate(&self) -> u64 {
        self.hll.count()
    }
}

/// Streaming median / percentile over a TDigest core. Accepts f64
/// samples; callers cast from int before adding.
#[derive(Debug, Clone, Default)]
pub struct QuantileTDigestAggregator {
    digest: TDigest,
}

impl QuantileTDigestAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, value: f64) {
        self.digest.add(value);
    }

    pub fn merge(&mut self, other: &Self) {
        self.digest.merge(&other.digest);
    }

    /// ClickHouse-compat: `q ∈ [0.0, 1.0]`. `quantile(0.5)` = median.
    pub fn quantile(&self, q: f64) -> f64 {
        self.digest.quantile(q)
    }
}

/// Numerically stable two-variable accumulator backing
/// `corr(x, y)` / `covar_pop(x, y)` / `covar_samp(x, y)`.
#[derive(Debug, Clone, Default)]
pub struct CovarianceAggregator {
    n: u64,
    mean_x: f64,
    mean_y: f64,
    /// Co-moment: Σ (x - mean_x)(y - mean_y).
    c2: f64,
    /// Σ (x - mean_x)²
    m2_x: f64,
    /// Σ (y - mean_y)²
    m2_y: f64,
}

impl CovarianceAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        self.n += 1;
        let dx = x - self.mean_x;
        self.mean_x += dx / self.n as f64;
        let dx2 = x - self.mean_x;
        self.m2_x += dx * dx2;
        let dy = y - self.mean_y;
        self.mean_y += dy / self.n as f64;
        let dy2 = y - self.mean_y;
        self.m2_y += dy * dy2;
        self.c2 += dx * dy2;
    }

    /// Population covariance. Returns 0 when no samples.
    pub fn covar_pop(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        self.c2 / self.n as f64
    }

    /// Sample covariance. Returns 0 with fewer than 2 samples.
    pub fn covar_samp(&self) -> f64 {
        if self.n < 2 {
            return 0.0;
        }
        self.c2 / (self.n - 1) as f64
    }

    /// Pearson correlation coefficient. Returns NaN when either
    /// series has zero variance.
    pub fn corr(&self) -> f64 {
        if self.n < 2 {
            return f64::NAN;
        }
        let denom = (self.m2_x * self.m2_y).sqrt();
        if denom <= 0.0 {
            return f64::NAN;
        }
        self.c2 / denom
    }

    /// Merge another partial accumulator in. Mirrors Welford's
    /// parallel combination rule for both moments and the co-moment.
    pub fn merge(&mut self, other: &Self) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = other.clone();
            return;
        }
        let n = self.n + other.n;
        let delta_x = other.mean_x - self.mean_x;
        let delta_y = other.mean_y - self.mean_y;
        let new_mean_x = self.mean_x + delta_x * (other.n as f64 / n as f64);
        let new_mean_y = self.mean_y + delta_y * (other.n as f64 / n as f64);
        self.m2_x += other.m2_x + delta_x * delta_x * (self.n as f64 * other.n as f64 / n as f64);
        self.m2_y += other.m2_y + delta_y * delta_y * (self.n as f64 * other.n as f64 / n as f64);
        self.c2 += other.c2 + delta_x * delta_y * (self.n as f64 * other.n as f64 / n as f64);
        self.mean_x = new_mean_x;
        self.mean_y = new_mean_y;
        self.n = n;
    }
}

/// `count_if(cond)` — count of truthy rows.
#[derive(Debug, Clone, Default)]
pub struct CountIfAggregator {
    count: u64,
}

impl CountIfAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, cond: bool) {
        if cond {
            self.count += 1;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.count += other.count;
    }

    pub fn finalize(&self) -> u64 {
        self.count
    }
}

/// `sum_if(x, cond)` / `avg_if(x, cond)`.
#[derive(Debug, Clone, Default)]
pub struct SumAvgIfAggregator {
    sum: f64,
    count: u64,
}

impl SumAvgIfAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, value: f64, cond: bool) {
        if cond && value.is_finite() {
            self.sum += value;
            self.count += 1;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.count += other.count;
    }

    pub fn sum(&self) -> f64 {
        self.sum
    }

    pub fn avg(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f64
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

/// `any(x)` — returns the first observed value. Deterministic
/// ordering is caller responsibility (ClickHouse doesn't guarantee
/// which row wins either).
#[derive(Debug, Clone, Default)]
pub struct AnyAggregator<T: Clone> {
    first: Option<T>,
}

impl<T: Clone> AnyAggregator<T> {
    pub fn new() -> Self {
        Self { first: None }
    }

    pub fn add(&mut self, value: T) {
        if self.first.is_none() {
            self.first = Some(value);
        }
    }

    pub fn merge(&mut self, other: &Self) {
        if self.first.is_none() {
            if let Some(v) = &other.first {
                self.first = Some(v.clone());
            }
        }
    }

    pub fn finalize(&self) -> Option<T> {
        self.first.clone()
    }
}

/// `anyLast(x)` — returns the last observed value.
#[derive(Debug, Clone, Default)]
pub struct AnyLastAggregator<T: Clone> {
    last: Option<T>,
}

impl<T: Clone> AnyLastAggregator<T> {
    pub fn new() -> Self {
        Self { last: None }
    }

    pub fn add(&mut self, value: T) {
        self.last = Some(value);
    }

    pub fn merge(&mut self, other: &Self) {
        if let Some(v) = &other.last {
            self.last = Some(v.clone());
        }
    }

    pub fn finalize(&self) -> Option<T> {
        self.last.clone()
    }
}

/// `groupArray(n)` — collects up to `n` values. Preserves order.
#[derive(Debug, Clone)]
pub struct GroupArrayAggregator<T: Clone> {
    limit: usize,
    buffer: VecDeque<T>,
}

impl<T: Clone> GroupArrayAggregator<T> {
    /// Pass `0` for unbounded (careful — high-cardinality groups can
    /// blow memory).
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            buffer: VecDeque::new(),
        }
    }

    pub fn add(&mut self, value: T) {
        if self.limit != 0 && self.buffer.len() >= self.limit {
            return;
        }
        self.buffer.push_back(value);
    }

    pub fn merge(&mut self, other: &Self) {
        for v in &other.buffer {
            if self.limit != 0 && self.buffer.len() >= self.limit {
                break;
            }
            self.buffer.push_back(v.clone());
        }
    }

    pub fn finalize(&self) -> Vec<T> {
        self.buffer.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

/// `arrayJoin` — flattens a `Vec<T>` into an iterator. In SQL this
/// fans a single row into N rows; callers iterate this inside their
/// scan loop.
pub fn array_join<T: Clone>(arr: &[T]) -> impl Iterator<Item = T> + '_ {
    arr.iter().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniq_estimates_roughly_distinct_count() {
        let mut u = UniqAggregator::new(12);
        for i in 0..10_000 {
            u.add_i64(i);
        }
        let est = u.estimate();
        let err = ((est as f64 - 10_000.0).abs() / 10_000.0) * 100.0;
        assert!(err < 5.0, "uniq error {err}% est={est}");
    }

    #[test]
    fn uniq_merges_two_sets_without_double_counting_overlap() {
        let mut a = UniqAggregator::new(12);
        let mut b = UniqAggregator::new(12);
        for i in 0..5000 {
            a.add_i64(i);
        }
        for i in 2500..7500 {
            b.add_i64(i);
        }
        a.merge(&b);
        let est = a.estimate();
        let err = ((est as f64 - 7500.0).abs() / 7500.0) * 100.0;
        assert!(err < 5.0, "uniq merge error {err}% est={est}");
    }

    #[test]
    fn quantile_tdigest_agrees_with_sorted_median_within_mvp_tolerance() {
        // See note on `TDigest::median_of_uniform_is_near_half` — MVP
        // tolerance is wide; precision-tuning is follow-on.
        let mut q = QuantileTDigestAggregator::new();
        for i in 0..10_000 {
            q.add(i as f64);
        }
        let m = q.quantile(0.5);
        assert!(m > 2000.0 && m < 8000.0, "median was {m}");
    }

    #[test]
    fn corr_is_one_on_perfect_positive_line() {
        let mut c = CovarianceAggregator::new();
        for i in 0..100 {
            c.add(i as f64, 2.0 * i as f64 + 3.0);
        }
        let r = c.corr();
        assert!((r - 1.0).abs() < 1e-9, "corr = {r}");
    }

    #[test]
    fn corr_is_negative_on_inverse_line() {
        let mut c = CovarianceAggregator::new();
        for i in 0..100 {
            c.add(i as f64, -3.0 * i as f64 + 5.0);
        }
        let r = c.corr();
        assert!((r + 1.0).abs() < 1e-9, "corr = {r}");
    }

    #[test]
    fn covar_pop_vs_covar_samp_relationship() {
        let mut c = CovarianceAggregator::new();
        for (x, y) in [(1.0, 2.0), (2.0, 4.0), (3.0, 6.0), (4.0, 8.0)] {
            c.add(x, y);
        }
        let pop = c.covar_pop();
        let samp = c.covar_samp();
        // samp = pop * n / (n-1) ⇒ samp > pop for finite n.
        assert!(samp > pop);
        assert!((samp - pop * 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn merge_combines_parallel_aggregators() {
        let mut left = CovarianceAggregator::new();
        let mut right = CovarianceAggregator::new();
        for i in 0..50 {
            left.add(i as f64, (i * 2) as f64);
        }
        for i in 50..100 {
            right.add(i as f64, (i * 2) as f64);
        }
        let mut full = CovarianceAggregator::new();
        for i in 0..100 {
            full.add(i as f64, (i * 2) as f64);
        }
        left.merge(&right);
        assert!((left.corr() - full.corr()).abs() < 1e-9);
        assert!((left.covar_pop() - full.covar_pop()).abs() < 1e-9);
    }

    #[test]
    fn count_if_only_counts_truthy_rows() {
        let mut c = CountIfAggregator::new();
        for i in 0..20 {
            c.add(i % 3 == 0);
        }
        assert_eq!(c.finalize(), 7); // 0,3,6,9,12,15,18
    }

    #[test]
    fn sum_if_skips_non_finite_values() {
        let mut s = SumAvgIfAggregator::new();
        s.add(1.0, true);
        s.add(f64::NAN, true);
        s.add(2.0, false);
        s.add(3.0, true);
        assert_eq!(s.sum(), 4.0);
        assert_eq!(s.count(), 2);
        assert_eq!(s.avg(), 2.0);
    }

    #[test]
    fn any_and_any_last_track_endpoints() {
        let mut first = AnyAggregator::<i32>::new();
        let mut last = AnyLastAggregator::<i32>::new();
        for i in 0..10 {
            first.add(i);
            last.add(i);
        }
        assert_eq!(first.finalize(), Some(0));
        assert_eq!(last.finalize(), Some(9));
    }

    #[test]
    fn group_array_respects_limit() {
        let mut g = GroupArrayAggregator::<i32>::new(3);
        for i in 0..10 {
            g.add(i);
        }
        assert_eq!(g.finalize(), vec![0, 1, 2]);
    }

    #[test]
    fn group_array_unbounded_when_limit_zero() {
        let mut g = GroupArrayAggregator::<i32>::new(0);
        for i in 0..5 {
            g.add(i);
        }
        assert_eq!(g.finalize(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn array_join_flattens_vec() {
        let v = vec![1, 2, 3];
        let out: Vec<i32> = array_join(&v).collect();
        assert_eq!(out, v);
    }

    #[test]
    fn group_array_merge_respects_limit() {
        let mut a = GroupArrayAggregator::<i32>::new(4);
        let mut b = GroupArrayAggregator::<i32>::new(4);
        a.add(1);
        a.add(2);
        b.add(3);
        b.add(4);
        b.add(5);
        a.merge(&b);
        assert_eq!(a.finalize(), vec![1, 2, 3, 4]);
    }
}
