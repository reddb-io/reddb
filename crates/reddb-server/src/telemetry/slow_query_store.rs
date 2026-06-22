//! Bounded in-memory ring buffer for slow-query telemetry events.
//!
//! This is the operational-telemetry substrate for slow queries
//! (ADR 0060, §2 "Operational events"). A ring of fixed capacity
//! evicts the oldest record on overflow — cardinality and retention are
//! bounded by construction.
//!
//! Privacy contract (ADR 0060, §5): tenant and identity are stored as
//! keyed FNV-1a hashes, never as raw strings. The raw SQL is the
//! caller-supplied redacted string; fingerprinting tightens this in a
//! follow-up slice.
//!
//! The `read()` method is the pure read-model layer — no filesystem
//! coupling — and is unit-tested over synthetic records in this file.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Process-local hash key
// ---------------------------------------------------------------------------

static HASH_KEY: OnceLock<u64> = OnceLock::new();

fn hash_key() -> u64 {
    *HASH_KEY.get_or_init(|| {
        // Process-local seed: timestamp XOR'd with PID. Not secret-quality
        // randomness but sufficient for operational grouping as per ADR 0060 §5
        // ("The keyed hash secret is process/deployment-scoped config").
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xcafe_babe_dead_beef)
            ^ (std::process::id() as u64).wrapping_mul(0x517cc1b727220a95)
    })
}

/// Hash `label` with the process-local key (FNV-1a keyed on `key`).
pub(crate) fn hash_label(label: &str) -> u64 {
    hash_label_with_key(label, hash_key())
}

fn hash_label_with_key(label: &str, key: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    // XOR the key into the FNV offset basis so different keys produce
    // different hash spaces for the same input.
    let mut h = key ^ 0xcbf2_9ce4_8422_2325;
    for byte in label.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

// ---------------------------------------------------------------------------
// SlowQueryEvent
// ---------------------------------------------------------------------------

/// A single slow-query operational telemetry event.
///
/// `tenant_hash` and `identity_hash` are keyed FNV-1a digests of the
/// raw tenant / identity strings (ADR 0060, §5). Raw values are never
/// stored by the substrate.
#[derive(Debug, Clone)]
pub struct SlowQueryEvent {
    pub ts_ms: u64,
    pub kind: &'static str,
    pub duration_ms: u64,
    /// Caller-supplied redacted SQL (literals collapsed by the producer).
    pub sql_redacted: String,
    /// Keyed hash of the raw tenant label.
    pub tenant_hash: u64,
    /// Keyed hash of the raw identity label.
    pub identity_hash: u64,
}

// ---------------------------------------------------------------------------
// SlowQueryFilter
// ---------------------------------------------------------------------------

/// Parameters for the pure read-model filter over recent slow-query events.
///
/// All fields are optional; an absent field means "no constraint."
/// `limit` defaults to [`DEFAULT_READ_LIMIT`] when `None`.
#[derive(Debug, Default, Clone)]
pub struct SlowQueryFilter {
    /// Maximum number of events to return (most-recent first).
    pub limit: Option<usize>,
    /// Return only events with `ts_ms >= since_ms`.
    pub since_ms: Option<u64>,
    /// Return only events with `duration_ms >= min_duration_ms`.
    pub min_duration_ms: Option<u64>,
    /// Return only events whose `kind` equals this exact string.
    pub kind: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// SlowQueryStore
// ---------------------------------------------------------------------------

/// Default per-class ring capacity (ADR 0060, §3: "last 10k slow queries").
pub const DEFAULT_CAP: usize = 10_000;

/// Default read limit when `SlowQueryFilter::limit` is absent.
const DEFAULT_READ_LIMIT: usize = 100;

/// Bounded ring buffer holding the most-recent `cap` slow-query events.
///
/// Thread-safe via a `Mutex<VecDeque>`. The lock is held only for
/// above-threshold, sampled events so contention is minimal.
pub struct SlowQueryStore {
    ring: Mutex<VecDeque<SlowQueryEvent>>,
    cap: usize,
}

impl SlowQueryStore {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            ring: Mutex::new(VecDeque::with_capacity(cap.min(1024))),
            cap,
        })
    }

    /// Append an event; evicts the oldest record when the ring is at capacity.
    pub fn push(&self, event: SlowQueryEvent) {
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= self.cap {
                ring.pop_front();
            }
            ring.push_back(event);
        }
    }

    /// Return recent events, most-recent first, applying `filter`.
    ///
    /// This is the pure read-model layer; it never touches the filesystem
    /// and holds the lock only for the duration of the linear scan.
    pub fn read(&self, filter: &SlowQueryFilter) -> Vec<SlowQueryEvent> {
        let limit = filter.limit.unwrap_or(DEFAULT_READ_LIMIT);
        let Ok(ring) = self.ring.lock() else {
            return vec![];
        };

        ring.iter()
            .rev()
            .filter(|e| {
                if let Some(since) = filter.since_ms {
                    if e.ts_ms < since {
                        return false;
                    }
                }
                if let Some(min_dur) = filter.min_duration_ms {
                    if e.duration_ms < min_dur {
                        return false;
                    }
                }
                if let Some(kind) = filter.kind {
                    if e.kind != kind {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Number of events currently in the ring.
    pub fn len(&self) -> usize {
        self.ring.lock().map(|r| r.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Tests — pure filter layer, zero filesystem coupling
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn event(ts_ms: u64, kind: &'static str, duration_ms: u64) -> SlowQueryEvent {
        SlowQueryEvent {
            ts_ms,
            kind,
            duration_ms,
            sql_redacted: format!("SELECT {ts_ms} FROM t"),
            tenant_hash: hash_label_with_key("tenant_a", 0xdead_beef),
            identity_hash: hash_label_with_key("user_1", 0xdead_beef),
        }
    }

    fn filled(events: &[(u64, &'static str, u64)]) -> Arc<SlowQueryStore> {
        let store = SlowQueryStore::new(DEFAULT_CAP);
        for &(ts, kind, dur) in events {
            store.push(event(ts, kind, dur));
        }
        store
    }

    #[test]
    fn empty_store_returns_empty() {
        let store = SlowQueryStore::new(DEFAULT_CAP);
        assert!(store.read(&SlowQueryFilter::default()).is_empty());
    }

    #[test]
    fn results_most_recent_first() {
        let store = filled(&[
            (1000, "select", 100),
            (2000, "select", 100),
            (3000, "select", 100),
        ]);
        let result = store.read(&SlowQueryFilter::default());
        assert_eq!(result.len(), 3);
        assert!(result[0].ts_ms >= result[1].ts_ms);
        assert!(result[1].ts_ms >= result[2].ts_ms);
    }

    #[test]
    fn limit_returns_n_most_recent() {
        let store = filled(&[
            (1000, "select", 100),
            (2000, "select", 100),
            (3000, "select", 100),
            (4000, "select", 100),
            (5000, "select", 100),
        ]);
        let result = store.read(&SlowQueryFilter {
            limit: Some(2),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].ts_ms, 5000);
        assert_eq!(result[1].ts_ms, 4000);
    }

    #[test]
    fn since_ms_excludes_older_events() {
        let store = filled(&[
            (1000, "select", 100),
            (2000, "select", 100),
            (3000, "select", 100),
        ]);
        let result = store.read(&SlowQueryFilter {
            since_ms: Some(2000),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
        for e in &result {
            assert!(e.ts_ms >= 2000, "ts_ms {} below since_ms", e.ts_ms);
        }
    }

    #[test]
    fn min_duration_ms_excludes_fast_queries() {
        let store = filled(&[
            (1000, "select", 50),
            (2000, "select", 200),
            (3000, "select", 500),
        ]);
        let result = store.read(&SlowQueryFilter {
            min_duration_ms: Some(200),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
        for e in &result {
            assert!(e.duration_ms >= 200, "duration {} below min", e.duration_ms);
        }
    }

    #[test]
    fn kind_filter_returns_only_matching() {
        let store = filled(&[
            (1000, "select", 100),
            (2000, "insert", 100),
            (3000, "select", 100),
            (4000, "delete", 100),
        ]);
        let result = store.read(&SlowQueryFilter {
            kind: Some("select"),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
        for e in &result {
            assert_eq!(e.kind, "select");
        }
    }

    #[test]
    fn combined_filters_are_conjunctive() {
        let store = filled(&[
            (1000, "select", 100),
            (2000, "select", 300),
            (3000, "insert", 300),
            (4000, "select", 300),
        ]);
        let result = store.read(&SlowQueryFilter {
            since_ms: Some(2000),
            min_duration_ms: Some(300),
            kind: Some("select"),
            limit: Some(10),
        });
        assert_eq!(result.len(), 2);
        for e in &result {
            assert_eq!(e.kind, "select");
            assert!(e.ts_ms >= 2000);
            assert!(e.duration_ms >= 300);
        }
    }

    #[test]
    fn ring_evicts_oldest_on_overflow() {
        let store = SlowQueryStore::new(3);
        for i in 0..5u64 {
            store.push(event(i * 1000, "select", 100));
        }
        assert_eq!(store.len(), 3);
        let result = store.read(&SlowQueryFilter::default());
        let tss: Vec<u64> = result.iter().map(|e| e.ts_ms).collect();
        assert!(!tss.contains(&0), "ts=0 should have been evicted");
        assert!(!tss.contains(&1000), "ts=1000 should have been evicted");
        assert!(tss.contains(&4000), "ts=4000 must be present");
    }

    #[test]
    fn default_limit_caps_read() {
        let store = SlowQueryStore::new(DEFAULT_CAP);
        for i in 0..(DEFAULT_READ_LIMIT + 50) as u64 {
            store.push(event(i * 1000, "select", 100));
        }
        assert_eq!(
            store.read(&SlowQueryFilter::default()).len(),
            DEFAULT_READ_LIMIT
        );
    }

    #[test]
    fn hash_stable_same_key() {
        assert_eq!(
            hash_label_with_key("my-tenant", 0xdead_beef),
            hash_label_with_key("my-tenant", 0xdead_beef),
        );
    }

    #[test]
    fn hash_different_inputs_differ() {
        assert_ne!(
            hash_label_with_key("tenant_a", 0xdead_beef),
            hash_label_with_key("tenant_b", 0xdead_beef),
        );
    }

    #[test]
    fn hash_different_keys_differ() {
        assert_ne!(
            hash_label_with_key("tenant", 0x1111),
            hash_label_with_key("tenant", 0x2222),
        );
    }
}
