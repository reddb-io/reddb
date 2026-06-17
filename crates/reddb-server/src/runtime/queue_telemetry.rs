//! Queue lifecycle telemetry — slice 10 of issue #527.
//!
//! Process-local Prometheus counters per ADR-0017 that the
//! `QueueLifecycle` Module bumps on every state transition. Rendered onto the
//! `/metrics` body alongside the rest of the engine's exposition.
//!
//! Series exposed:
//!
//! - `queue_delivered_total{queue, group, mode}` — counter, one
//!   increment per message handed back from a deliver/read call.
//! - `queue_acked_total{queue, group, mode}` — counter, one
//!   increment per `ACK`.
//! - `queue_nacked_total{queue, group, mode, outcome=dlq|retry|drop}`
//!   — counter, increment per NACK tagged with the lifecycle's
//!   retirement choice.
//! - `queue_pending_gauge{queue, group}` — gauge, scraped live
//!   from `red_queue_meta` at render time so it can't drift from
//!   the source of truth. Not stored in this module.
//!
//! Cardinality is bounded by the catalog: queue + group + mode are
//! all values the operator already created. No payload data leaks
//! into label space.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// NACK retirement outcome — mirrors the lifecycle's
/// `RetirementOutcome` but uses the short Prometheus label string
/// dashboards already think in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NackOutcomeLabel {
    Retry,
    Dlq,
    Drop,
}

impl NackOutcomeLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NackOutcomeLabel::Retry => "retry",
            NackOutcomeLabel::Dlq => "dlq",
            NackOutcomeLabel::Drop => "drop",
        }
    }
}

/// Terminal outcome of a `QUEUE READ … WAIT` park lifecycle — slice D
/// of PRD #718 (#729). One outcome counter increment per `started`
/// increment; the histogram captures started→resolved duration for the
/// same lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcomeLabel {
    /// A notify fired and the post-wake re-probe handed back a message.
    Woken,
    /// The WAIT budget elapsed without a delivery.
    Timeout,
    /// The registry was cancelled (shutdown drain).
    Cancelled,
}

impl WaitOutcomeLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            WaitOutcomeLabel::Woken => "woken",
            WaitOutcomeLabel::Timeout => "timed_out",
            WaitOutcomeLabel::Cancelled => "cancelled",
        }
    }
}

/// Histogram bucket upper bounds in milliseconds for `queue_wait_duration_ms`.
/// Le-buckets in classical Prometheus shape (each value is `le=<n>`); a
/// final `+Inf` bucket is emitted by the renderer.
pub const WAIT_DURATION_BUCKETS_MS: &[u64] = &[10, 50, 100, 500, 1_000, 5_000, 30_000, 60_000];

#[derive(Debug, Default)]
struct CounterCell {
    value: AtomicU64,
}

/// Per-`(scope, queue)` histogram snapshot for `queue_wait_duration_ms`.
/// `bucket_counts[i]` is the cumulative count of samples whose value
/// is `<= WAIT_DURATION_BUCKETS_MS[i]`; the renderer emits the
/// trailing `+Inf` bucket as `count` itself.
#[derive(Debug, Clone, Default)]
pub struct WaitDurationHistogram {
    pub bucket_counts: Vec<u64>,
    pub sum_ms: u64,
    pub count: u64,
}

/// Materialised snapshot returned to the metrics handler. Read-only
/// — pricing the lock once per scrape is cheap relative to the
/// rest of `/metrics`.
#[derive(Debug, Clone, Default)]
pub struct QueueTelemetrySnapshot {
    pub delivered: Vec<((String, String, String), u64)>,
    pub acked: Vec<((String, String, String), u64)>,
    pub nacked: Vec<((String, String, String, &'static str), u64)>,
    /// Slice D of PRD #718 (#729) — `queue_wait_started_total` per
    /// `(scope, queue)`.
    pub wait_started: Vec<((String, String), u64)>,
    /// `queue_wait_woken_total` per `(scope, queue)`.
    pub wait_woken: Vec<((String, String), u64)>,
    /// `queue_wait_timed_out_total` per `(scope, queue)`.
    pub wait_timed_out: Vec<((String, String), u64)>,
    /// `queue_wait_cancelled_total` per `(scope, queue)`.
    pub wait_cancelled: Vec<((String, String), u64)>,
    /// `queue_wait_duration_ms` histogram per `(scope, queue)`.
    pub wait_duration: Vec<((String, String), WaitDurationHistogram)>,
}

#[derive(Debug)]
struct HistogramCell {
    bucket_counts: Vec<AtomicU64>,
    sum_ms: AtomicU64,
    count: AtomicU64,
}

impl Default for HistogramCell {
    fn default() -> Self {
        Self {
            bucket_counts: (0..WAIT_DURATION_BUCKETS_MS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            sum_ms: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl HistogramCell {
    fn observe(&self, value_ms: u64) {
        for (i, upper) in WAIT_DURATION_BUCKETS_MS.iter().enumerate() {
            if value_ms <= *upper {
                self.bucket_counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_ms.fetch_add(value_ms, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> WaitDurationHistogram {
        WaitDurationHistogram {
            bucket_counts: self
                .bucket_counts
                .iter()
                .map(|c| c.load(Ordering::Relaxed))
                .collect(),
            sum_ms: self.sum_ms.load(Ordering::Relaxed),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct QueueTelemetryCounters {
    /// (queue, group, mode) → count. `Mutex<BTreeMap>` keeps the
    /// render path deterministic for the integration test and is
    /// cheap relative to a queue operation; the hot path lives on
    /// the atomic inside the cell.
    delivered: Mutex<BTreeMap<(String, String, String), CounterCell>>,
    acked: Mutex<BTreeMap<(String, String, String), CounterCell>>,
    /// (queue, group, mode, outcome) → count.
    nacked: Mutex<BTreeMap<(String, String, String, &'static str), CounterCell>>,
    /// Slice D of PRD #718 (#729) — `(scope, queue)` keyed wait
    /// counters and histogram. Scope is the registry scope (today
    /// `current_tenant().unwrap_or_default()`).
    wait_started: Mutex<BTreeMap<(String, String), CounterCell>>,
    wait_woken: Mutex<BTreeMap<(String, String), CounterCell>>,
    wait_timed_out: Mutex<BTreeMap<(String, String), CounterCell>>,
    wait_cancelled: Mutex<BTreeMap<(String, String), CounterCell>>,
    wait_duration: Mutex<BTreeMap<(String, String), HistogramCell>>,
}

impl QueueTelemetryCounters {
    pub(crate) fn record_delivered(&self, queue: &str, group: &str, mode: &str, n: u64) {
        if n == 0 {
            return;
        }
        let key = (queue.to_string(), group.to_string(), mode.to_string());
        let mut map = self.delivered.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key)
            .or_default()
            .value
            .fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn record_acked(&self, queue: &str, group: &str, mode: &str) {
        let key = (queue.to_string(), group.to_string(), mode.to_string());
        let mut map = self.acked.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key)
            .or_default()
            .value
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_nacked(
        &self,
        queue: &str,
        group: &str,
        mode: &str,
        outcome: NackOutcomeLabel,
    ) {
        let key = (
            queue.to_string(),
            group.to_string(),
            mode.to_string(),
            outcome.as_str(),
        );
        let mut map = self.nacked.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key)
            .or_default()
            .value
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn delivered_snapshot(&self) -> Vec<((String, String, String), u64)> {
        let map = self.delivered.lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .map(|(k, v)| (k.clone(), v.value.load(Ordering::Relaxed)))
            .collect()
    }

    pub(crate) fn acked_snapshot(&self) -> Vec<((String, String, String), u64)> {
        let map = self.acked.lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .map(|(k, v)| (k.clone(), v.value.load(Ordering::Relaxed)))
            .collect()
    }

    pub(crate) fn nacked_snapshot(&self) -> Vec<((String, String, String, &'static str), u64)> {
        let map = self.nacked.lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .map(|(k, v)| (k.clone(), v.value.load(Ordering::Relaxed)))
            .collect()
    }

    // -----------------------------------------------------------------
    // Wait counters & histogram — slice D of PRD #718 (#729).
    // -----------------------------------------------------------------

    /// One increment when a WAIT lifecycle enters the park loop (after
    /// the first non-blocking probe returned empty). Pairs 1:1 with
    /// exactly one terminal outcome counter increment.
    pub(crate) fn record_wait_started(&self, scope: &str, queue: &str) {
        let key = (scope.to_string(), queue.to_string());
        let mut map = self.wait_started.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key)
            .or_default()
            .value
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Records the terminal outcome plus the started→resolved duration
    /// in the histogram. Single point so the counter and histogram can
    /// never drift apart.
    pub(crate) fn record_wait_outcome(
        &self,
        scope: &str,
        queue: &str,
        outcome: WaitOutcomeLabel,
        duration_ms: u64,
    ) {
        let key = (scope.to_string(), queue.to_string());
        let map = match outcome {
            WaitOutcomeLabel::Woken => &self.wait_woken,
            WaitOutcomeLabel::Timeout => &self.wait_timed_out,
            WaitOutcomeLabel::Cancelled => &self.wait_cancelled,
        };
        {
            let mut g = map.lock().unwrap_or_else(|p| p.into_inner());
            g.entry(key.clone())
                .or_default()
                .value
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut h = self.wait_duration.lock().unwrap_or_else(|p| p.into_inner());
        h.entry(key).or_default().observe(duration_ms);
    }

    fn pair_snapshot(
        map: &Mutex<BTreeMap<(String, String), CounterCell>>,
    ) -> Vec<((String, String), u64)> {
        let m = map.lock().unwrap_or_else(|p| p.into_inner());
        m.iter()
            .map(|(k, v)| (k.clone(), v.value.load(Ordering::Relaxed)))
            .collect()
    }

    pub(crate) fn wait_started_snapshot(&self) -> Vec<((String, String), u64)> {
        Self::pair_snapshot(&self.wait_started)
    }

    pub(crate) fn wait_woken_snapshot(&self) -> Vec<((String, String), u64)> {
        Self::pair_snapshot(&self.wait_woken)
    }

    pub(crate) fn wait_timed_out_snapshot(&self) -> Vec<((String, String), u64)> {
        Self::pair_snapshot(&self.wait_timed_out)
    }

    pub(crate) fn wait_cancelled_snapshot(&self) -> Vec<((String, String), u64)> {
        Self::pair_snapshot(&self.wait_cancelled)
    }

    pub(crate) fn wait_duration_snapshot(&self) -> Vec<((String, String), WaitDurationHistogram)> {
        let map = self.wait_duration.lock().unwrap_or_else(|p| p.into_inner());
        map.iter().map(|(k, v)| (k.clone(), v.snapshot())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_accumulates_per_label_set() {
        let c = QueueTelemetryCounters::default();
        c.record_delivered("orders", "workers", "work", 1);
        c.record_delivered("orders", "workers", "work", 2);
        c.record_delivered("orders", "audit", "work", 1);
        let snap = c.delivered_snapshot();
        assert_eq!(snap.len(), 2);
        let by_group: BTreeMap<String, u64> =
            snap.into_iter().map(|((_, g, _), n)| (g, n)).collect();
        assert_eq!(by_group["workers"], 3);
        assert_eq!(by_group["audit"], 1);
    }

    #[test]
    fn wait_started_and_outcomes_increment_independently() {
        let c = QueueTelemetryCounters::default();
        c.record_wait_started("", "q1");
        c.record_wait_started("", "q1");
        c.record_wait_started("", "q2");
        c.record_wait_outcome("", "q1", WaitOutcomeLabel::Woken, 42);
        c.record_wait_outcome("", "q1", WaitOutcomeLabel::Timeout, 200);
        c.record_wait_outcome("", "q2", WaitOutcomeLabel::Cancelled, 5);

        let started: BTreeMap<_, _> = c.wait_started_snapshot().into_iter().collect();
        assert_eq!(started[&("".to_string(), "q1".to_string())], 2);
        assert_eq!(started[&("".to_string(), "q2".to_string())], 1);

        let woken: BTreeMap<_, _> = c.wait_woken_snapshot().into_iter().collect();
        assert_eq!(woken[&("".to_string(), "q1".to_string())], 1);
        assert!(!woken.contains_key(&("".to_string(), "q2".to_string())));

        let timed: BTreeMap<_, _> = c.wait_timed_out_snapshot().into_iter().collect();
        assert_eq!(timed[&("".to_string(), "q1".to_string())], 1);

        let cancelled: BTreeMap<_, _> = c.wait_cancelled_snapshot().into_iter().collect();
        assert_eq!(cancelled[&("".to_string(), "q2".to_string())], 1);
    }

    #[test]
    fn wait_duration_histogram_buckets_observations_correctly() {
        let c = QueueTelemetryCounters::default();
        // 42ms -> falls in <=50 and all higher buckets.
        c.record_wait_outcome("", "q", WaitOutcomeLabel::Woken, 42);
        // 750ms -> <=1000 and higher.
        c.record_wait_outcome("", "q", WaitOutcomeLabel::Timeout, 750);

        let hist: BTreeMap<_, _> = c.wait_duration_snapshot().into_iter().collect();
        let h = &hist[&("".to_string(), "q".to_string())];
        assert_eq!(h.count, 2);
        assert_eq!(h.sum_ms, 792);
        // buckets = [10, 50, 100, 500, 1000, 5000, 30000, 60000]
        assert_eq!(
            h.bucket_counts[0], 0,
            "0ms <=10 should be 0 (42 and 750 both above)"
        );
        assert_eq!(h.bucket_counts[1], 1, "<=50 catches the 42 sample");
        assert_eq!(h.bucket_counts[2], 1, "<=100 still only the 42 sample");
        assert_eq!(h.bucket_counts[3], 1, "<=500 still only 42");
        assert_eq!(h.bucket_counts[4], 2, "<=1000 catches both");
        assert_eq!(h.bucket_counts[7], 2, "<=60000 catches both");
    }

    #[test]
    fn nacked_separates_by_outcome() {
        let c = QueueTelemetryCounters::default();
        c.record_nacked("q", "g", "work", NackOutcomeLabel::Retry);
        c.record_nacked("q", "g", "work", NackOutcomeLabel::Retry);
        c.record_nacked("q", "g", "work", NackOutcomeLabel::Dlq);
        let snap = c.nacked_snapshot();
        let map: BTreeMap<&'static str, u64> =
            snap.into_iter().map(|((_, _, _, o), n)| (o, n)).collect();
        assert_eq!(map["retry"], 2);
        assert_eq!(map["dlq"], 1);
        assert!(!map.contains_key("drop"));
    }
}
