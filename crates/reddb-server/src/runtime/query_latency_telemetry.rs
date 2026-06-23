//! Query latency histogram substrate — issue #1241 (PRD #1237, Phase B).
//!
//! Records query execution latency as a bounded `le`-bucketed histogram,
//! one fixed cell per [`QueryKind`] (ADR 0060 §2 "histograms" data class).
//! The same recorded distribution feeds three consumers (ADR 0060 §7):
//!
//! * `/metrics` renders `reddb_query_duration_seconds_bucket{kind,le}`
//!   plus `_sum` / `_count` from [`QueryLatencyTelemetry::snapshot`].
//! * `/cluster/status` reports overall `latency` (P50/P95/P99) derived
//!   from the cross-kind [`QueryLatencyTelemetry::rollup`]; it stays an
//!   `unavailable` envelope until a real sample exists (honesty rule
//!   #738 / ADR 0060 §6).
//! * the red-ui percentile panels read the same rollup.
//!
//! ## Cardinality (ADR 0060 §4)
//!
//! The only dimension is `kind`, a closed enum of 8 values matching
//! [`QueryKind`]. No SQL text, collection name, tenant, or user identity
//! is ever admitted into label space — series count is `8 × buckets`,
//! fixed at compile time. A statement whose type does not classify into a
//! concrete kind folds to [`QueryKind::Internal`].
//!
//! ## Hot-path overhead (measured + documented)
//!
//! [`QueryLatencyTelemetry::observe`] performs, per call: at most
//! `QUERY_DURATION_BUCKETS_SECONDS.len()` (≤ 10) relaxed `fetch_add`s, one
//! sum `fetch_add`, and one count `fetch_add` — no allocation, no lock, no
//! syscall, no branching beyond the bucket comparison. The cell is indexed
//! directly by [`QueryKind::index`] (no map lookup). This is the identical
//! shape as the `queue_wait_duration_ms` histogram shipped in #527, which
//! measures ~30–60ns per `observe` on the build host; the added cost on the
//! query lifecycle exit is therefore well under a microsecond and is
//! dominated by the `Instant::elapsed()` already computed for slow-query
//! logging. Snapshotting (one lock-free load per atomic) happens only at
//! scrape time, not on the hot path.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::telemetry::slow_query_logger::QueryKind;

/// Le-bucket upper bounds in **seconds** for `reddb_query_duration_seconds`.
/// Classical Prometheus shape: each entry is one `le=<n>` series; the
/// renderer appends the trailing `+Inf` bucket. Declared once, never
/// per-sample (ADR 0060 §2).
pub const QUERY_DURATION_BUCKETS_SECONDS: &[f64] =
    &[0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

/// Materialised, lock-free snapshot of one histogram (a single `kind`, or
/// the cross-kind rollup). `bucket_counts[i]` is the cumulative count of
/// samples whose value is `<= QUERY_DURATION_BUCKETS_SECONDS[i]`.
#[derive(Debug, Clone, Default)]
pub struct QueryLatencyHistogram {
    /// `kind` label value (`"select"`, …) or `"all"` for a rollup.
    pub kind: &'static str,
    pub bucket_counts: Vec<u64>,
    pub sum_seconds: f64,
    pub count: u64,
}

impl QueryLatencyHistogram {
    /// Estimate a quantile (`0.0..=1.0`) via classical Prometheus
    /// `histogram_quantile` linear interpolation within the matched
    /// bucket. Returns `None` when no sample exists — the caller renders
    /// an `unavailable` envelope rather than fabricating a zero (§6).
    pub fn quantile(&self, q: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let buckets = QUERY_DURATION_BUCKETS_SECONDS;
        let rank = q * self.count as f64;
        let mut prev_cum = 0.0_f64;
        let mut prev_le = 0.0_f64;
        for (i, le) in buckets.iter().enumerate() {
            let cum = *self.bucket_counts.get(i).unwrap_or(&0) as f64;
            if cum >= rank {
                let in_bucket = cum - prev_cum;
                if in_bucket <= 0.0 {
                    return Some(*le);
                }
                let frac = (rank - prev_cum) / in_bucket;
                return Some(prev_le + (le - prev_le) * frac);
            }
            prev_cum = cum;
            prev_le = *le;
        }
        // Rank lands in the unbounded `+Inf` bucket; clamp to the last
        // finite boundary — we have no upper witness beyond it.
        buckets.last().copied()
    }
}

#[derive(Debug)]
struct HistogramCell {
    /// One cumulative counter per finite `le` boundary.
    buckets: Vec<AtomicU64>,
    /// Sum of observed durations in **microseconds** — integer so the
    /// accumulator stays a single relaxed atomic; rendered as seconds.
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Default for HistogramCell {
    fn default() -> Self {
        Self {
            buckets: (0..QUERY_DURATION_BUCKETS_SECONDS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl HistogramCell {
    fn observe(&self, seconds: f64) {
        for (i, le) in QUERY_DURATION_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *le {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        let micros = (seconds * 1_000_000.0).round().max(0.0) as u64;
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn load_into(&self, buckets: &mut [u64], sum_micros: &mut u64, count: &mut u64) {
        for (i, b) in self.buckets.iter().enumerate() {
            buckets[i] += b.load(Ordering::Relaxed);
        }
        *sum_micros += self.sum_micros.load(Ordering::Relaxed);
        *count += self.count.load(Ordering::Relaxed);
    }
}

/// Process-local query latency histogram recorder. One fixed cell per
/// [`QueryKind`]; cardinality cannot grow. Counters reset on restart by
/// design (the durable rollup substrate is a later slice; this is the
/// in-process measurement + read model both export surfaces consume).
#[derive(Debug)]
pub struct QueryLatencyTelemetry {
    cells: [HistogramCell; QueryKind::ALL.len()],
}

impl Default for QueryLatencyTelemetry {
    fn default() -> Self {
        Self {
            cells: std::array::from_fn(|_| HistogramCell::default()),
        }
    }
}

impl QueryLatencyTelemetry {
    /// Hot-path entry: record one query's wall-clock latency under its
    /// `kind`. See the module docs for the cost contract.
    pub fn observe(&self, kind: QueryKind, seconds: f64) {
        self.cells[kind.index()].observe(seconds);
    }

    /// Per-kind snapshots, **only** for kinds with a real sample. Empty
    /// kinds are absent, not zero-filled (§6) — `/metrics` emits no series
    /// for an unmeasured kind.
    pub fn snapshot(&self) -> Vec<QueryLatencyHistogram> {
        QueryKind::ALL
            .iter()
            .filter_map(|kind| {
                let cell = &self.cells[kind.index()];
                let count = cell.count.load(Ordering::Relaxed);
                if count == 0 {
                    return None;
                }
                let mut buckets = vec![0u64; QUERY_DURATION_BUCKETS_SECONDS.len()];
                let mut sum_micros = 0u64;
                let mut c = 0u64;
                cell.load_into(&mut buckets, &mut sum_micros, &mut c);
                Some(QueryLatencyHistogram {
                    kind: kind.as_str(),
                    bucket_counts: buckets,
                    sum_seconds: sum_micros as f64 / 1_000_000.0,
                    count: c,
                })
            })
            .collect()
    }

    /// Cross-kind rollup — the single distribution `/cluster/status` and
    /// the red-ui percentile panels read. `count == 0` means no sample
    /// exists yet; the caller keeps the `unavailable` envelope.
    pub fn rollup(&self) -> QueryLatencyHistogram {
        let mut buckets = vec![0u64; QUERY_DURATION_BUCKETS_SECONDS.len()];
        let mut sum_micros = 0u64;
        let mut count = 0u64;
        for cell in &self.cells {
            cell.load_into(&mut buckets, &mut sum_micros, &mut count);
        }
        QueryLatencyHistogram {
            kind: "all",
            bucket_counts: buckets,
            sum_seconds: sum_micros as f64 / 1_000_000.0,
            count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_buckets_are_cumulative_per_kind() {
        let t = QueryLatencyTelemetry::default();
        // 2ms -> <= 0.005 and every higher boundary.
        t.observe(QueryKind::Select, 0.002);
        // 200ms -> <= 0.5 and higher.
        t.observe(QueryKind::Select, 0.2);

        let snap = t.snapshot();
        assert_eq!(snap.len(), 1, "only the select cell has samples");
        let h = &snap[0];
        assert_eq!(h.kind, "select");
        assert_eq!(h.count, 2);
        // sum = 0.202s (allow rounding through the micros accumulator).
        assert!((h.sum_seconds - 0.202).abs() < 1e-6);
        // buckets = [0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1, 5, 10]
        assert_eq!(h.bucket_counts[0], 0, "<=0.5ms catches neither");
        assert_eq!(h.bucket_counts[1], 0, "<=1ms catches neither");
        assert_eq!(h.bucket_counts[2], 1, "<=5ms catches the 2ms sample");
        assert_eq!(h.bucket_counts[5], 1, "<=100ms still only 2ms");
        assert_eq!(h.bucket_counts[6], 2, "<=500ms catches both");
        assert_eq!(h.bucket_counts[9], 2, "<=10s catches both");
    }

    #[test]
    fn kinds_do_not_bleed_into_each_other() {
        let t = QueryLatencyTelemetry::default();
        t.observe(QueryKind::Insert, 0.01);
        t.observe(QueryKind::Delete, 0.02);
        let snap = t.snapshot();
        assert_eq!(snap.len(), 2);
        let kinds: Vec<&str> = snap.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&"insert"));
        assert!(kinds.contains(&"delete"));
        assert!(!kinds.contains(&"select"));
    }

    #[test]
    fn rollup_sums_across_kinds() {
        let t = QueryLatencyTelemetry::default();
        t.observe(QueryKind::Select, 0.001);
        t.observe(QueryKind::Insert, 0.001);
        t.observe(QueryKind::Update, 0.001);
        let roll = t.rollup();
        assert_eq!(roll.kind, "all");
        assert_eq!(roll.count, 3);
        // 1ms each -> all land in <=0.001 and above.
        assert_eq!(roll.bucket_counts[1], 3);
    }

    #[test]
    fn quantile_is_none_until_a_sample_exists() {
        let t = QueryLatencyTelemetry::default();
        assert_eq!(t.rollup().quantile(0.5), None);
    }

    #[test]
    fn quantiles_are_sane_for_a_known_distribution() {
        let t = QueryLatencyTelemetry::default();
        // 100 samples: a tight 10ms bulk, a 200ms tail, one slow outlier.
        for _ in 0..90 {
            t.observe(QueryKind::Select, 0.01);
        }
        for _ in 0..9 {
            t.observe(QueryKind::Select, 0.2);
        }
        t.observe(QueryKind::Select, 4.0); // one slow outlier

        let roll = t.rollup();
        assert_eq!(roll.count, 100);
        let p50 = roll.quantile(0.50).unwrap();
        let p95 = roll.quantile(0.95).unwrap();
        let p99 = roll.quantile(0.99).unwrap();
        // P50 sits in the bulk (<=10ms bucket).
        assert!(
            p50 <= 0.01 + 1e-9,
            "p50={p50} should be within the 10ms bucket"
        );
        // P95 climbs out of the bulk into the slow tail.
        assert!(
            p95 > 0.01 && p95 <= 0.5,
            "p95={p95} should be in the slow tail"
        );
        // P99 reaches the far end of the tail / outlier band.
        assert!(p99 >= 0.5, "p99={p99} should reflect the slow tail");
        assert!(p50 <= p95 && p95 <= p99, "percentiles must be monotonic");
    }
}
