//! Continuous aggregates — incremental time-bucket materialisations.
//!
//! A continuous aggregate keeps the result of
//!
//! ```sql
//! SELECT time_bucket('5m', ts) AS bucket,
//!        <aggs...>
//! FROM metrics
//! GROUP BY bucket;
//! ```
//!
//! materialised so dashboards never re-scan the parent chunks.
//! Refresh is **incremental**: the daemon tracks
//! `last_refreshed_bucket`, reads only rows whose bucket is ≥ that
//! watermark, and appends / upserts the new buckets. Old buckets
//! stay immutable — matches the contract users expect from
//! Timescale's `continuous_aggregate` + `refresh_continuous_aggregate`.
//!
//! This module owns:
//! * [`ContinuousAggregateSpec`] — declarative definition
//!   (`CREATE CONTINUOUS AGGREGATE`)
//! * [`ContinuousAggregateState`] — the materialised bucket map +
//!   watermark
//! * [`ContinuousAggregateEngine`] — in-memory registry + refresh
//!   driver that accepts a source-scan callback
//!
//! Physical storage + SQL dispatch wire in during the sprint that
//! follows (needs the `MaterializedViewCache` extension for the
//! `TimeWindow` refresh policy); tests here pin the refresh
//! arithmetic end-to-end.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use super::aggregation::AggregationType;
use super::retention::parse_duration_ns;

/// Column to aggregate + aggregation type. Matches the
/// `AggregationType` surface the time-bucket code already uses.
#[derive(Debug, Clone)]
pub struct ContinuousAggregateColumn {
    pub alias: String,
    pub source_column: String,
    pub agg: AggregationType,
}

#[derive(Debug, Clone)]
pub struct ContinuousAggregateSpec {
    pub name: String,
    /// Source time-series / hypertable this aggregate reads from.
    pub source: String,
    /// Size of the time bucket in nanoseconds.
    pub bucket_size_ns: u64,
    /// Aggregated columns.
    pub columns: Vec<ContinuousAggregateColumn>,
    /// Lag (ns) between now() and the newest bucket the refresh
    /// daemon is willing to materialise. Matches Timescale's
    /// `start_offset` — stops us from materialising a bucket whose
    /// source rows are still landing.
    pub refresh_lag_ns: u64,
    /// Maximum span (ns) a single refresh will materialise at once.
    /// Timescale calls this `max_interval_per_job`.
    pub max_interval_per_job_ns: u64,
}

impl ContinuousAggregateSpec {
    /// Convenience constructor from string durations.
    pub fn from_durations(
        name: impl Into<String>,
        source: impl Into<String>,
        bucket: &str,
        columns: Vec<ContinuousAggregateColumn>,
        refresh_lag: &str,
        max_interval_per_job: &str,
    ) -> Option<Self> {
        Some(Self {
            name: name.into(),
            source: source.into(),
            bucket_size_ns: parse_duration_ns(bucket)?.max(1),
            columns,
            refresh_lag_ns: parse_duration_ns(refresh_lag).unwrap_or(0),
            max_interval_per_job_ns: parse_duration_ns(max_interval_per_job).unwrap_or(u64::MAX),
        })
    }

    /// Align timestamp to bucket floor.
    pub fn bucket_start(&self, ts_ns: u64) -> u64 {
        (ts_ns / self.bucket_size_ns) * self.bucket_size_ns
    }

    pub fn bucket_end_exclusive(&self, ts_ns: u64) -> u64 {
        self.bucket_start(ts_ns).saturating_add(self.bucket_size_ns)
    }
}

/// Per-bucket aggregator state. Stores the intermediate state each
/// aggregation type needs to combine additional rows when the
/// refresh daemon picks up where it left off.
#[derive(Debug, Clone, Default)]
pub struct BucketState {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub first: Option<f64>,
    pub last: Option<f64>,
    pub any_observed: bool,
}

impl BucketState {
    pub fn new() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.count += 1;
        self.sum += value;
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        if self.first.is_none() {
            self.first = Some(value);
        }
        self.last = Some(value);
        self.any_observed = true;
    }

    pub fn merge(&mut self, other: &BucketState) {
        if !other.any_observed {
            return;
        }
        self.count += other.count;
        self.sum += other.sum;
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
        if self.first.is_none() {
            self.first = other.first;
        }
        if other.last.is_some() {
            self.last = other.last;
        }
        self.any_observed = true;
    }

    pub fn value(&self, agg: AggregationType) -> f64 {
        if !self.any_observed {
            return 0.0;
        }
        match agg {
            AggregationType::Count => self.count as f64,
            AggregationType::Sum => self.sum,
            AggregationType::Avg => {
                if self.count == 0 {
                    0.0
                } else {
                    self.sum / self.count as f64
                }
            }
            AggregationType::Min => self.min,
            AggregationType::Max => self.max,
            AggregationType::First => self.first.unwrap_or(0.0),
            AggregationType::Last => self.last.unwrap_or(0.0),
        }
    }
}

/// Materialised bucket keyed by bucket-start timestamp.
#[derive(Debug, Clone, Default)]
pub struct ContinuousAggregateState {
    /// Per-alias per-bucket state.
    buckets: BTreeMap<u64, HashMap<String, BucketState>>,
    /// Every bucket ≤ this watermark is considered "closed and
    /// materialised". Starts at 0 so the first refresh processes
    /// everything.
    last_refreshed_bucket_ns: u64,
}

impl ContinuousAggregateState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_refreshed_bucket_ns(&self) -> u64 {
        self.last_refreshed_bucket_ns
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Lookup a single bucket's value for the given alias. Returns
    /// `None` when the bucket has not been materialised yet or the
    /// alias is unknown.
    pub fn query(&self, bucket_start_ns: u64, alias: &str, agg: AggregationType) -> Option<f64> {
        self.buckets
            .get(&bucket_start_ns)
            .and_then(|row| row.get(alias))
            .map(|state| state.value(agg))
    }

    /// List every materialised bucket in ascending order.
    pub fn buckets(&self) -> Vec<u64> {
        self.buckets.keys().copied().collect()
    }
}

/// A row emitted by the source-scan callback during refresh.
#[derive(Debug, Clone)]
pub struct RefreshPoint {
    pub ts_ns: u64,
    /// `alias → value`. Allows one pass to feed every aggregate.
    pub values: HashMap<String, f64>,
}

/// Source-scan callback the engine uses to stream rows from the
/// parent during refresh. Receiving the `[start, end)` window lets
/// the callback restrict its scan — the core optimisation that
/// makes refreshes incremental rather than full.
pub type ContinuousAggregateSource = Arc<dyn Fn(&str, u64, u64) -> Vec<RefreshPoint> + Send + Sync>;

#[derive(Clone)]
pub struct ContinuousAggregateEngine {
    inner: Arc<Mutex<EngineInner>>,
}

struct EngineInner {
    specs: HashMap<String, ContinuousAggregateSpec>,
    states: HashMap<String, ContinuousAggregateState>,
}

impl std::fmt::Debug for ContinuousAggregateEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        f.debug_struct("ContinuousAggregateEngine")
            .field("aggregates", &guard.specs.len())
            .finish()
    }
}

impl ContinuousAggregateEngine {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(EngineInner {
                specs: HashMap::new(),
                states: HashMap::new(),
            })),
        }
    }

    pub fn register(&self, spec: ContinuousAggregateSpec) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.insert(spec.name.clone(), spec.clone());
        guard
            .states
            .entry(spec.name.clone())
            .or_insert_with(ContinuousAggregateState::new);
    }

    pub fn drop_aggregate(&self, name: &str) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.remove(name);
        guard.states.remove(name);
    }

    pub fn list(&self) -> Vec<ContinuousAggregateSpec> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.values().cloned().collect()
    }

    pub fn state(&self, name: &str) -> Option<ContinuousAggregateState> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.states.get(name).cloned()
    }

    /// Refresh a single aggregate: consult `now_ns`, compute the
    /// `[start, end)` window that can safely land (bounded by
    /// `refresh_lag_ns` and `max_interval_per_job_ns`), call the
    /// source callback, and fold the returned points into the
    /// materialised buckets. Returns the number of points absorbed.
    pub fn refresh(&self, name: &str, now_ns: u64, source: &ContinuousAggregateSource) -> u64 {
        let spec = match self.get_spec(name) {
            Some(s) => s,
            None => return 0,
        };
        let state_snapshot = self.get_state(name).unwrap_or_default();

        // End of the window: `now - refresh_lag`, aligned to bucket.
        let latest_safe = now_ns.saturating_sub(spec.refresh_lag_ns);
        let end_bucket = spec.bucket_start(latest_safe);
        let start_bucket = state_snapshot.last_refreshed_bucket_ns;

        if end_bucket <= start_bucket {
            return 0;
        }

        // Cap by max_interval_per_job so no single refresh runs
        // unbounded when the aggregate has been idle for ages.
        let max_span = spec.max_interval_per_job_ns;
        let end_bucket = if end_bucket.saturating_sub(start_bucket) > max_span {
            start_bucket.saturating_add(max_span)
        } else {
            end_bucket
        };

        let points = source(&spec.source, start_bucket, end_bucket);
        let absorbed = points.len() as u64;

        // Apply.
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let state = guard
            .states
            .entry(name.to_string())
            .or_insert_with(ContinuousAggregateState::new);

        for point in points {
            if point.ts_ns < start_bucket || point.ts_ns >= end_bucket {
                continue; // callback may return out-of-window rows; ignore
            }
            let bucket_start = spec.bucket_start(point.ts_ns);
            let row = state
                .buckets
                .entry(bucket_start)
                .or_insert_with(HashMap::new);
            for col in &spec.columns {
                if let Some(value) = point.values.get(&col.alias) {
                    row.entry(col.alias.clone())
                        .or_insert_with(BucketState::new)
                        .observe(*value);
                }
            }
        }
        state.last_refreshed_bucket_ns = end_bucket;
        absorbed
    }

    fn get_spec(&self, name: &str) -> Option<ContinuousAggregateSpec> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.get(name).cloned()
    }

    fn get_state(&self, name: &str) -> Option<ContinuousAggregateState> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.states.get(name).cloned()
    }
}

impl Default for ContinuousAggregateEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60_000_000_000;
    const HOUR: u64 = 60 * MINUTE;

    fn spec() -> ContinuousAggregateSpec {
        ContinuousAggregateSpec {
            name: "five_min_load".into(),
            source: "metrics".into(),
            bucket_size_ns: 5 * MINUTE,
            columns: vec![
                ContinuousAggregateColumn {
                    alias: "avg_load".into(),
                    source_column: "load".into(),
                    agg: AggregationType::Avg,
                },
                ContinuousAggregateColumn {
                    alias: "max_load".into(),
                    source_column: "load".into(),
                    agg: AggregationType::Max,
                },
            ],
            refresh_lag_ns: 0,
            max_interval_per_job_ns: u64::MAX,
        }
    }

    fn points(values: Vec<(u64, f64)>) -> ContinuousAggregateSource {
        Arc::new(move |_source, start, end| {
            values
                .iter()
                .filter(|(ts, _)| *ts >= start && *ts < end)
                .map(|(ts, v)| {
                    let mut map = HashMap::new();
                    map.insert("avg_load".to_string(), *v);
                    map.insert("max_load".to_string(), *v);
                    RefreshPoint {
                        ts_ns: *ts,
                        values: map,
                    }
                })
                .collect()
        })
    }

    #[test]
    fn refresh_fills_buckets_until_now_minus_lag() {
        let engine = ContinuousAggregateEngine::new();
        engine.register(spec());
        let source = points(vec![
            (0, 10.0),
            (MINUTE, 20.0),
            (5 * MINUTE, 5.0),
            (6 * MINUTE, 15.0),
        ]);
        let absorbed = engine.refresh("five_min_load", 15 * MINUTE, &source);
        assert_eq!(absorbed, 4);
        let state = engine.state("five_min_load").unwrap();
        let buckets = state.buckets();
        assert_eq!(buckets, vec![0, 5 * MINUTE]);
        assert!((state.query(0, "avg_load", AggregationType::Avg).unwrap() - 15.0).abs() < 1e-9);
        assert_eq!(
            state.query(0, "max_load", AggregationType::Max).unwrap(),
            20.0
        );
        assert_eq!(
            state
                .query(5 * MINUTE, "max_load", AggregationType::Max)
                .unwrap(),
            15.0
        );
    }

    #[test]
    fn refresh_is_incremental_across_two_calls() {
        let engine = ContinuousAggregateEngine::new();
        engine.register(spec());

        // First batch lives in the first bucket (0..5m).
        let source1 = points(vec![(MINUTE, 10.0), (2 * MINUTE, 20.0)]);
        engine.refresh("five_min_load", 5 * MINUTE, &source1);

        // Second batch appends to a new bucket (5m..10m) without
        // reprocessing the first.
        let source2 = points(vec![(6 * MINUTE, 100.0), (7 * MINUTE, 50.0)]);
        engine.refresh("five_min_load", 10 * MINUTE, &source2);

        let state = engine.state("five_min_load").unwrap();
        assert_eq!(state.bucket_count(), 2);
        assert_eq!(
            state
                .query(5 * MINUTE, "avg_load", AggregationType::Avg)
                .unwrap(),
            75.0
        );
    }

    #[test]
    fn refresh_respects_lag_window() {
        let engine = ContinuousAggregateEngine::new();
        let mut s = spec();
        s.refresh_lag_ns = 10 * MINUTE;
        engine.register(s);
        let source = points(vec![
            (0, 1.0),
            (MINUTE, 2.0),
            (5 * MINUTE, 3.0),
            (8 * MINUTE, 4.0),
        ]);
        // now = 12m, lag = 10m ⇒ window ends at 12m - 10m = 2m → bucket 0.
        let absorbed = engine.refresh("five_min_load", 12 * MINUTE, &source);
        // Points with ts in [0, 0) are all filtered.
        assert_eq!(absorbed, 0);
    }

    #[test]
    fn refresh_caps_work_per_job() {
        let engine = ContinuousAggregateEngine::new();
        let mut s = spec();
        s.max_interval_per_job_ns = 5 * MINUTE;
        engine.register(s);
        let source = points(vec![(0, 1.0), (5 * MINUTE, 2.0), (10 * MINUTE, 3.0)]);
        // Single refresh should only chew through 5 minutes of
        // buckets (0..5m), leaving the 5m/10m buckets for the next
        // cycle.
        engine.refresh("five_min_load", HOUR, &source);
        let state = engine.state("five_min_load").unwrap();
        assert_eq!(state.bucket_count(), 1);
        assert_eq!(state.last_refreshed_bucket_ns(), 5 * MINUTE);
    }

    #[test]
    fn refresh_of_unknown_aggregate_is_a_noop() {
        let engine = ContinuousAggregateEngine::new();
        let source: ContinuousAggregateSource = Arc::new(|_, _, _| Vec::new());
        assert_eq!(engine.refresh("does_not_exist", 0, &source), 0);
    }

    #[test]
    fn bucket_state_merges_cumulative_counts() {
        let mut a = BucketState::new();
        a.observe(1.0);
        a.observe(3.0);
        let mut b = BucketState::new();
        b.observe(5.0);
        a.merge(&b);
        assert_eq!(a.count, 3);
        assert_eq!(a.sum, 9.0);
        assert_eq!(a.min, 1.0);
        assert_eq!(a.max, 5.0);
    }

    #[test]
    fn spec_from_durations_parses_intervals() {
        let spec = ContinuousAggregateSpec::from_durations(
            "hourly",
            "metrics",
            "1h",
            vec![ContinuousAggregateColumn {
                alias: "c".into(),
                source_column: "v".into(),
                agg: AggregationType::Count,
            }],
            "5m",
            "1d",
        )
        .unwrap();
        assert_eq!(spec.bucket_size_ns, HOUR);
        assert_eq!(spec.refresh_lag_ns, 5 * MINUTE);
    }
}
