//! Heavy-hitters index — top-k frequent values per column.
//!
//! The cost-based planner needs more than distinct counts: when a column has
//! a handful of *very* frequent values and a long tail, equality selectivity
//! against `1 / distinct` dramatically underestimates those hot keys. A
//! heavy-hitters sketch surfaces the top-k values plus their approximate
//! frequencies, giving the planner per-value selectivity instead of a
//! uniform estimate.
//!
//! # Data structure
//!
//! - Backbone: [`CountMinSketch`] for per-key frequency estimates
//!   (never underestimates).
//! - Top-k cache: a small `BinaryHeap<Reverse<(count, key)>>` maintained on
//!   every [`HeavyHitters::observe`] call. Eviction happens when a new key
//!   has a higher CMS estimate than the smallest element in the heap.
//!
//! Implements [`IndexBase`] so the registry and planner consume it via the
//! same trait as every other index.
//!
//! # Memory
//!
//! Default sketch: 1000 × 5 counters (~40 KB). Top-k heap: `k` × (8 bytes
//! count + key bytes). Callers that care can pass custom CMS parameters via
//! [`HeavyHitters::with_params`].

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::{IndexBase, IndexKind, IndexStats};
use crate::storage::primitives::count_min_sketch::CountMinSketch;

/// Default `k` when callers don't specify one.
const DEFAULT_K: usize = 16;

/// Top-k frequent-value sketch.
pub struct HeavyHitters {
    name: String,
    k: usize,
    cms: CountMinSketch,
    /// Min-heap over observed keys ordered by their current estimate.
    /// Wrapped in `Reverse` so `peek` returns the weakest top-k entry.
    top: BinaryHeap<Reverse<(u64, Vec<u8>)>>,
    /// Total observations (all keys, including those never in top-k).
    total_observed: u64,
}

impl HeavyHitters {
    /// Create a heavy-hitters index with the default CMS size and
    /// [`DEFAULT_K`].
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_params(name, DEFAULT_K, 1000, 5)
    }

    /// Fully-configurable constructor. `k` is the top-k size,
    /// `width`/`depth` size the CMS (more width = tighter estimates).
    pub fn with_params(
        name: impl Into<String>,
        k: usize,
        cms_width: usize,
        cms_depth: usize,
    ) -> Self {
        Self {
            name: name.into(),
            k: k.max(1),
            cms: CountMinSketch::new(cms_width, cms_depth),
            top: BinaryHeap::new(),
            total_observed: 0,
        }
    }

    /// Record one observation of `key`. Frequency estimate and top-k cache
    /// are updated atomically — callers never see a stale state.
    pub fn observe(&mut self, key: &[u8]) {
        self.observe_n(key, 1);
    }

    /// Record `count` observations of `key`. Bulk-load variant.
    pub fn observe_n(&mut self, key: &[u8], count: u64) {
        if count == 0 {
            return;
        }
        self.cms.add(key, count);
        self.total_observed = self.total_observed.saturating_add(count);

        let estimate = self.cms.estimate(key);

        // Rebuild top-k lazily: if the key is already tracked we can't
        // update its heap entry in place, so drop stale entries and
        // re-insert. Small k keeps this cheap.
        let mut kept: Vec<(u64, Vec<u8>)> = self
            .top
            .drain()
            .map(|Reverse(pair)| pair)
            .filter(|(_, k)| k != key)
            .collect();
        kept.push((estimate, key.to_vec()));
        kept.sort_by(|a, b| b.0.cmp(&a.0));
        kept.truncate(self.k);
        self.top = kept.into_iter().map(Reverse).collect();
    }

    /// Return the current top-k entries, highest frequency first.
    pub fn top_k(&self) -> Vec<(Vec<u8>, u64)> {
        let mut out: Vec<(u64, Vec<u8>)> = self
            .top
            .iter()
            .map(|Reverse((c, k))| (*c, k.clone()))
            .collect();
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(c, k)| (k, c)).collect()
    }

    /// Estimate the frequency of a single key (never underestimates).
    pub fn estimate(&self, key: &[u8]) -> u64 {
        self.cms.estimate(key)
    }

    /// Total number of observations recorded (including keys outside the
    /// top-k).
    pub fn total_observed(&self) -> u64 {
        self.total_observed
    }

    /// Relative frequency of `key` as a fraction of all observations.
    /// Returns `0.0` for an empty sketch.
    pub fn relative_frequency(&self, key: &[u8]) -> f64 {
        if self.total_observed == 0 {
            return 0.0;
        }
        self.estimate(key) as f64 / self.total_observed as f64
    }

    /// Configured top-k capacity.
    pub fn k(&self) -> usize {
        self.k
    }

    /// Reset the sketch and the top-k cache.
    pub fn clear(&mut self) {
        self.cms.clear();
        self.top.clear();
        self.total_observed = 0;
    }
}

impl IndexBase for HeavyHitters {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> IndexKind {
        IndexKind::HeavyHitters
    }

    fn stats(&self) -> IndexStats {
        IndexStats {
            entries: self.total_observed as usize,
            // We don't know true distinct cardinality (that's HLL's job),
            // so report the top-k size as the visible key count.
            distinct_keys: self.top.len(),
            approx_bytes: 0,
            kind: IndexKind::HeavyHitters,
            has_bloom: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_and_tracks_top_k() {
        let mut hh = HeavyHitters::with_params("test", 3, 256, 4);
        for _ in 0..100 {
            hh.observe(b"alpha");
        }
        for _ in 0..50 {
            hh.observe(b"beta");
        }
        for _ in 0..10 {
            hh.observe(b"charlie");
        }
        for _ in 0..1 {
            hh.observe(b"delta");
        }

        let top = hh.top_k();
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].0, b"alpha".to_vec());
        assert!(top[0].1 >= 100);
        assert_eq!(top[1].0, b"beta".to_vec());
        assert!(top[1].1 >= 50);
        assert_eq!(top[2].0, b"charlie".to_vec());
    }

    #[test]
    fn estimate_never_underestimates() {
        let mut hh = HeavyHitters::with_params("test", 8, 1024, 4);
        for i in 0..500u32 {
            hh.observe(&i.to_be_bytes());
        }
        for i in 0..500u32 {
            assert!(hh.estimate(&i.to_be_bytes()) >= 1);
        }
    }

    #[test]
    fn relative_frequency_scales_with_total() {
        let mut hh = HeavyHitters::new("t");
        for _ in 0..400 {
            hh.observe(b"hot");
        }
        for _ in 0..100 {
            hh.observe(b"cold");
        }
        let hot = hh.relative_frequency(b"hot");
        let cold = hh.relative_frequency(b"cold");
        // CMS overestimates, so only sanity-check the ordering.
        assert!(hot > cold);
        assert!(hot >= 0.75);
    }

    #[test]
    fn skewed_distribution_surfaces_hot_keys() {
        let mut hh = HeavyHitters::with_params("t", 5, 4096, 5);
        // 3 hot keys + 1000 cold keys
        for _ in 0..1000 {
            hh.observe(b"hotA");
        }
        for _ in 0..800 {
            hh.observe(b"hotB");
        }
        for _ in 0..600 {
            hh.observe(b"hotC");
        }
        for i in 0..1000u32 {
            hh.observe(&i.to_be_bytes());
        }
        let top = hh.top_k();
        let top_keys: Vec<&[u8]> = top.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(top_keys.contains(&b"hotA".as_ref()));
        assert!(top_keys.contains(&b"hotB".as_ref()));
        assert!(top_keys.contains(&b"hotC".as_ref()));
    }

    #[test]
    fn observe_n_is_equivalent_to_looped_observe() {
        let mut a = HeavyHitters::with_params("a", 4, 512, 4);
        let mut b = HeavyHitters::with_params("b", 4, 512, 4);
        a.observe_n(b"bulk", 1000);
        for _ in 0..1000 {
            b.observe(b"bulk");
        }
        assert_eq!(a.estimate(b"bulk"), b.estimate(b"bulk"));
        assert_eq!(a.total_observed(), b.total_observed());
    }

    #[test]
    fn clear_resets_state() {
        let mut hh = HeavyHitters::new("t");
        hh.observe(b"x");
        hh.clear();
        assert_eq!(hh.total_observed(), 0);
        assert!(hh.top_k().is_empty());
        assert_eq!(hh.estimate(b"x"), 0);
    }

    #[test]
    fn stats_surface_totals_and_kind() {
        let mut hh = HeavyHitters::with_params("t", 4, 256, 3);
        for i in 0..50u32 {
            hh.observe(&i.to_be_bytes());
        }
        let s = hh.stats();
        assert_eq!(s.entries, 50);
        assert_eq!(s.kind, IndexKind::HeavyHitters);
        // With k=4, the heap is capped at 4 distinct tracked keys.
        assert!(s.distinct_keys <= 4);
    }

    #[test]
    fn zero_count_observation_is_noop() {
        let mut hh = HeavyHitters::new("t");
        hh.observe_n(b"ghost", 0);
        assert_eq!(hh.total_observed(), 0);
        assert!(hh.top_k().is_empty());
    }
}
