//! Concurrent claim telemetry.
//!
//! Process-local counters for `UPDATE ... CLAIM` observability. Labels are
//! bounded to `(collection, model)`; no predicate values, ids, consumers, or
//! query text enter metric cardinality.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Default)]
struct CounterCell {
    value: AtomicU64,
}

/// Materialised snapshot returned to `/metrics` and tests.
#[derive(Debug, Clone, Default)]
pub struct ClaimTelemetrySnapshot {
    pub attempts: Vec<((String, String), u64)>,
    pub successful: Vec<((String, String), u64)>,
    pub misses: Vec<((String, String), u64)>,
    pub skipped_locked: Vec<((String, String), u64)>,
}

#[derive(Debug, Default)]
pub(crate) struct ClaimTelemetryCounters {
    attempts: Mutex<BTreeMap<(String, String), CounterCell>>,
    successful: Mutex<BTreeMap<(String, String), CounterCell>>,
    misses: Mutex<BTreeMap<(String, String), CounterCell>>,
    skipped_locked: Mutex<BTreeMap<(String, String), CounterCell>>,
}

impl ClaimTelemetryCounters {
    pub(crate) fn record_attempt(&self, collection: &str, model: &str) {
        increment(&self.attempts, collection, model, 1);
    }

    pub(crate) fn record_successful(&self, collection: &str, model: &str, count: u64) {
        increment(&self.successful, collection, model, count);
    }

    pub(crate) fn record_miss(&self, collection: &str, model: &str) {
        increment(&self.misses, collection, model, 1);
    }

    pub(crate) fn record_skipped_locked(&self, collection: &str, model: &str, count: u64) {
        increment(&self.skipped_locked, collection, model, count);
    }

    pub(crate) fn snapshot(&self) -> ClaimTelemetrySnapshot {
        ClaimTelemetrySnapshot {
            attempts: snapshot_counter(&self.attempts),
            successful: snapshot_counter(&self.successful),
            misses: snapshot_counter(&self.misses),
            skipped_locked: snapshot_counter(&self.skipped_locked),
        }
    }
}

fn increment(
    counter: &Mutex<BTreeMap<(String, String), CounterCell>>,
    collection: &str,
    model: &str,
    count: u64,
) {
    if count == 0 {
        return;
    }
    let key = (collection.to_string(), model.to_string());
    let mut map = counter.lock().unwrap_or_else(|p| p.into_inner());
    map.entry(key)
        .or_default()
        .value
        .fetch_add(count, Ordering::Relaxed);
}

fn snapshot_counter(
    counter: &Mutex<BTreeMap<(String, String), CounterCell>>,
) -> Vec<((String, String), u64)> {
    let map = counter.lock().unwrap_or_else(|p| p.into_inner());
    map.iter()
        .map(|(k, v)| (k.clone(), v.value.load(Ordering::Relaxed)))
        .collect()
}
