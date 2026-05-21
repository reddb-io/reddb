//! Queue lifecycle telemetry — slice 10 of issue #527.
//!
//! Process-local Prometheus counters per ADR-0017 that the
//! `QueueLifecycle` Module (plus the legacy `queue_delivery` path
//! that still serves the user-facing `QUEUE READ` / `ACK` / `NACK`
//! today) bumps on every state transition. Rendered onto the
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

#[derive(Debug, Default)]
struct CounterCell {
    value: AtomicU64,
}

/// Materialised snapshot returned to the metrics handler. Read-only
/// — pricing the lock once per scrape is cheap relative to the
/// rest of `/metrics`.
#[derive(Debug, Clone, Default)]
pub struct QueueTelemetrySnapshot {
    pub delivered: Vec<((String, String, String), u64)>,
    pub acked: Vec<((String, String, String), u64)>,
    pub nacked: Vec<((String, String, String, &'static str), u64)>,
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
