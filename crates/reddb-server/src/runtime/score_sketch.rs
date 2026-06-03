//! Approximate score sketch for leaderboard rank — the *tail* half of the
//! hybrid rank (issue #923 / ADR 0035).
//!
//! The exact, MVCC-correct rank is served only for the bounded top-K head
//! (`ranking_descriptor_catalog` + `impl_core::compute_exact_head_rank`).
//! For an entry below that head a leaderboard wants "you're in the top X%",
//! not an exact position — and an exact global rank under MVCC is either
//! wrong-semantics or expensive (ADR 0035). This module is the explicitly
//! *approximate* aggregate that fills that gap.
//!
//! ## Engine: equi-width histogram
//!
//! ADR 0035 deliberately leaves the sketch engine (t-digest vs equi-depth
//! histogram vs count-min) open. We pick the simplest structure that gives
//! a *documented, testable* error band: a fixed-resolution **equi-width
//! histogram** over `[min, max]` with [`DEFAULT_BUCKETS`] buckets.
//!
//! Each bucket holds the count of scores that fell in its half-open slice
//! `[min + i·w, min + (i+1)·w)` (the top bucket is inclusive of `max`),
//! where `w = (max − min) / B`. An approximate rank for score `s` sums the
//! buckets strictly beyond `s`'s bucket and interpolates linearly *within*
//! `s`'s bucket (assuming a uniform spread across the bucket width).
//!
//! ## Error band (documented — criterion 3)
//!
//! The only loss is the within-bucket interpolation, so the absolute rank
//! error is bounded by the population of the bucket the target lands in,
//! and therefore by the largest bucket:
//!
//! ```text
//! |approx_rank(s) − exact_rank(s)|  ≤  max_i counts[i]  ≤  total
//! ```
//!
//! For a distribution spread across all `B` buckets the largest bucket is
//! ≈ `total / B`, so the error band tightens to roughly `total / B`. See
//! [`ScoreSketch::max_bucket_count`] for the live bound and the unit tests
//! for the empirical check against a uniform distribution.

use crate::utils::json::{parse_json, JsonValue};

/// Default number of histogram buckets. 256 keeps the error band at
/// ≈ `total / 256` for a spread distribution while staying tiny to persist.
pub const DEFAULT_BUCKETS: usize = 256;

/// An approximate score distribution as an equi-width histogram.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreSketch {
    /// Lowest observed score.
    min: f64,
    /// Highest observed score.
    max: f64,
    /// Per-bucket counts, `counts.len()` == bucket resolution.
    counts: Vec<u64>,
    /// Total number of scores folded in (`sum(counts)`).
    total: u64,
}

/// An approximate position within the ranking, derived from the sketch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApproxRank {
    /// 1-based approximate rank (1 == best). Always labeled approximate by
    /// the surface — never presented as an exact head position.
    pub rank: u64,
    /// Share of the population this entry ranks at-or-ahead of, in `0..=100`.
    /// Higher means nearer the top of the board.
    pub percentile: f64,
}

impl ScoreSketch {
    /// Build a sketch from raw scores with [`DEFAULT_BUCKETS`] resolution.
    pub fn from_scores(scores: &[f64]) -> Self {
        Self::from_scores_with_buckets(scores, DEFAULT_BUCKETS)
    }

    /// Build a sketch with an explicit bucket resolution (≥ 1).
    pub fn from_scores_with_buckets(scores: &[f64], buckets: usize) -> Self {
        let buckets = buckets.max(1);
        let clean: Vec<f64> = scores.iter().copied().filter(|s| s.is_finite()).collect();
        if clean.is_empty() {
            return Self {
                min: 0.0,
                max: 0.0,
                counts: vec![0; buckets],
                total: 0,
            };
        }
        let mut min = clean[0];
        let mut max = clean[0];
        for &s in &clean[1..] {
            if s < min {
                min = s;
            }
            if s > max {
                max = s;
            }
        }
        let mut counts = vec![0u64; buckets];
        for &s in &clean {
            counts[Self::bucket_index(min, max, buckets, s)] += 1;
        }
        Self {
            min,
            max,
            counts,
            total: clean.len() as u64,
        }
    }

    /// Bucket index a score falls in, clamped to `[0, buckets - 1]`.
    fn bucket_index(min: f64, max: f64, buckets: usize, score: f64) -> usize {
        if max <= min {
            return 0;
        }
        let width = (max - min) / buckets as f64;
        let raw = ((score - min) / width).floor();
        if raw < 0.0 {
            0
        } else if raw as usize >= buckets {
            buckets - 1
        } else {
            raw as usize
        }
    }

    /// Total scores folded into the sketch.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The largest single-bucket population — the live absolute rank-error
    /// bound (see the module-level error-band note).
    pub fn max_bucket_count(&self) -> u64 {
        self.counts.iter().copied().max().unwrap_or(0)
    }

    /// Estimated number of scores strictly *better* than `score`, where
    /// "better" follows `descending` (higher-is-better when `true`). Sums
    /// the buckets fully beyond `score`'s bucket and interpolates the
    /// fraction of `score`'s own bucket that lies beyond it.
    fn estimate_better(&self, score: f64, descending: bool) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let buckets = self.counts.len();
        if self.max <= self.min {
            // Degenerate distribution: every score is equal, nobody is
            // strictly better.
            return 0.0;
        }
        let idx = Self::bucket_index(self.min, self.max, buckets, score);
        let width = (self.max - self.min) / buckets as f64;
        let bucket_low = self.min + idx as f64 * width;
        let bucket_high = bucket_low + width;

        let mut better = 0.0;
        if descending {
            // Higher is better: buckets above idx, plus the slice of idx's
            // bucket above `score`.
            for &c in &self.counts[idx + 1..] {
                better += c as f64;
            }
            let frac_above = ((bucket_high - score) / width).clamp(0.0, 1.0);
            better += self.counts[idx] as f64 * frac_above;
        } else {
            // Lower is better: buckets below idx, plus the slice below.
            for &c in &self.counts[..idx] {
                better += c as f64;
            }
            let frac_below = ((score - bucket_low) / width).clamp(0.0, 1.0);
            better += self.counts[idx] as f64 * frac_below;
        }
        better
    }

    /// Approximate rank + percentile for `score`. `None` when the sketch is
    /// empty. RANK semantics: `rank = 1 + round(scores strictly better)`,
    /// so the best score is rank 1.
    pub fn approx_rank(&self, score: f64, descending: bool) -> Option<ApproxRank> {
        if self.total == 0 {
            return None;
        }
        let better = self.estimate_better(score, descending).round();
        let better = better.clamp(0.0, (self.total - 1) as f64) as u64;
        let rank = better + 1;
        // Share of the population this entry ranks at-or-ahead of: everyone
        // except those strictly better. 100 == top of the board.
        let at_or_ahead = self.total - better;
        let percentile = (at_or_ahead as f64 / self.total as f64) * 100.0;
        Some(ApproxRank { rank, percentile })
    }

    // ───────────────────────── persistence ─────────────────────────

    /// Serialize to the compact JSON shape persisted in `red_config`.
    pub fn to_json(&self) -> crate::serde_json::Value {
        let mut obj = crate::serde_json::Map::new();
        obj.insert(
            "min".to_string(),
            crate::serde_json::Value::Number(self.min),
        );
        obj.insert(
            "max".to_string(),
            crate::serde_json::Value::Number(self.max),
        );
        obj.insert(
            "total".to_string(),
            crate::serde_json::Value::Number(self.total as f64),
        );
        obj.insert(
            "counts".to_string(),
            crate::serde_json::Value::Array(
                self.counts
                    .iter()
                    .map(|c| crate::serde_json::Value::Number(*c as f64))
                    .collect(),
            ),
        );
        crate::serde_json::Value::Object(obj)
    }

    /// Parse a sketch from its persisted JSON string. Returns `None` for
    /// malformed input so a corrupt record degrades to "no sketch" rather
    /// than poisoning a read.
    pub fn from_json_str(raw: &str) -> Option<Self> {
        let parsed = parse_json(raw).ok()?;
        let obj = parsed.as_object()?;
        let lookup = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        let min = lookup("min").and_then(JsonValue::as_f64)?;
        let max = lookup("max").and_then(JsonValue::as_f64)?;
        let total = lookup("total").and_then(JsonValue::as_f64)? as u64;
        let counts: Vec<u64> = lookup("counts")
            .and_then(JsonValue::as_array)?
            .iter()
            .filter_map(|v| v.as_f64().map(|n| n as u64))
            .collect();
        if counts.is_empty() {
            return None;
        }
        Some(Self {
            min,
            max,
            counts,
            total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sketch_has_no_rank() {
        let sketch = ScoreSketch::from_scores(&[]);
        assert_eq!(sketch.total(), 0);
        assert!(sketch.approx_rank(10.0, true).is_none());
    }

    #[test]
    fn degenerate_all_equal_ranks_first() {
        let sketch = ScoreSketch::from_scores(&[50.0; 20]);
        let r = sketch.approx_rank(50.0, true).expect("rank");
        assert_eq!(r.rank, 1, "nobody is strictly better in a flat field");
        assert!((r.percentile - 100.0).abs() < 1e-9);
    }

    #[test]
    fn descending_best_is_rank_one_worst_is_last() {
        // Scores 1..=100, higher is better.
        let scores: Vec<f64> = (1..=100).map(|n| n as f64).collect();
        let sketch = ScoreSketch::from_scores(&scores);

        let best = sketch.approx_rank(100.0, true).expect("rank");
        assert_eq!(best.rank, 1);
        assert!(best.percentile > 99.0, "top score sits near 100%");

        let worst = sketch.approx_rank(1.0, true).expect("rank");
        assert_eq!(worst.rank, 100, "lowest score ranks last of 100");
    }

    #[test]
    fn ascending_lower_is_better() {
        // Latency-style: lower is better.
        let scores: Vec<f64> = (1..=100).map(|n| n as f64).collect();
        let sketch = ScoreSketch::from_scores(&scores);
        let fastest = sketch.approx_rank(1.0, false).expect("rank");
        assert_eq!(fastest.rank, 1, "lowest score ranks first when ascending");
        let slowest = sketch.approx_rank(100.0, false).expect("rank");
        assert_eq!(slowest.rank, 100);
    }

    /// Criterion 3 — the estimate falls inside the documented error band
    /// against a known (uniform) distribution.
    #[test]
    fn uniform_distribution_within_documented_error_band() {
        let n = 1000u64;
        let scores: Vec<f64> = (1..=n).map(|v| v as f64).collect();
        let sketch = ScoreSketch::from_scores(&scores); // 256 buckets

        // Documented bound: |approx − exact| ≤ max bucket population.
        let band = sketch.max_bucket_count();
        assert!(
            band <= (n / DEFAULT_BUCKETS as u64) + 2,
            "spread distribution should keep the largest bucket near total/B; got {band}"
        );

        // Check every score: exact descending rank of score v (1..=n) is
        // (n - v + 1). The sketch estimate must stay within the band.
        for v in 1..=n {
            let exact = n - v + 1;
            let approx = sketch.approx_rank(v as f64, true).expect("rank").rank;
            let delta = exact.abs_diff(approx);
            assert!(
                delta <= band,
                "score {v}: approx {approx} vs exact {exact} exceeds band {band}"
            );
        }
    }

    #[test]
    fn json_round_trips() {
        let scores: Vec<f64> = (1..=50).map(|n| n as f64 * 2.0).collect();
        let sketch = ScoreSketch::from_scores(&scores);
        let encoded = sketch.to_json().to_string();
        let decoded = ScoreSketch::from_json_str(&encoded).expect("round-trip");
        assert_eq!(decoded, sketch);
    }

    #[test]
    fn corrupt_json_degrades_to_none() {
        assert!(ScoreSketch::from_json_str("not json").is_none());
        assert!(ScoreSketch::from_json_str("{\"min\":0}").is_none());
    }
}
