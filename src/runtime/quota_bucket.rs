//! Per-caller QPS quotas via token bucket (PLAN.md Phase 4.4).
//!
//! Operator-visible knobs:
//!   * `RED_MAX_QPS_PER_CALLER` — sustained tokens-per-second
//!     refilled into each principal's bucket. `0` / unset disables
//!     the quota entirely (back-compat).
//!   * `RED_QPS_BURST` — bucket capacity. Defaults to
//!     `RED_MAX_QPS_PER_CALLER` so a steady-state caller never
//!     trips the gate; spikes drain the burst then get throttled.
//!
//! ## Principal resolution
//!
//! The HTTP layer derives a principal label per request:
//!   * Bearer-token caller → `bearer:<sha256-prefix>` (don't log
//!     the raw token).
//!   * Replica RPC → `replica:<id>` from the `replica_id` field.
//!   * Anonymous → `anon`.
//!
//! Each unique principal gets its own bucket. Buckets are evicted
//! lazily when they sit at capacity for `EVICT_AFTER` so
//! short-lived dev clients don't pile up.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EVICT_AFTER: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct Bucket {
    /// Current token count, may go fractional during refill.
    tokens: f64,
    /// Last refill timestamp.
    last_refill: Instant,
    /// Total denials this bucket has issued — surfaced via
    /// `reddb_quota_rejected_total{principal}`.
    rejected: u64,
}

#[derive(Debug)]
pub struct QuotaBucket {
    /// Tokens refilled per second. `0.0` disables the quota
    /// (every consume returns `Granted`).
    rate_per_sec: f64,
    /// Bucket capacity (max burst).
    burst: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaOutcome {
    /// Token consumed; caller may proceed.
    Granted,
    /// Bucket empty. Caller should return 429 with `Retry-After`
    /// roughly equal to `1 / rate_per_sec` seconds.
    Throttled,
    /// Quota disabled entirely (operator didn't set
    /// `RED_MAX_QPS_PER_CALLER`).
    NotConfigured,
}

impl QuotaBucket {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            rate_per_sec: rate_per_sec.max(0.0),
            burst: burst.max(rate_per_sec).max(0.0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let rate = std::env::var("RED_MAX_QPS_PER_CALLER")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(0.0);
        let burst = std::env::var("RED_QPS_BURST")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(rate);
        Self::new(rate, burst)
    }

    pub fn is_configured(&self) -> bool {
        self.rate_per_sec > 0.0
    }

    /// Try to consume one token for `principal`. Returns the
    /// outcome; the caller maps it to HTTP 429 / wire backoff.
    pub fn consume(&self, principal: &str) -> QuotaOutcome {
        if !self.is_configured() {
            return QuotaOutcome::NotConfigured;
        }
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("quota bucket mutex");
        let bucket = buckets.entry(principal.to_string()).or_insert(Bucket {
            tokens: self.burst,
            last_refill: now,
            rejected: 0,
        });
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate_per_sec).min(self.burst);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            QuotaOutcome::Granted
        } else {
            bucket.rejected = bucket.rejected.saturating_add(1);
            QuotaOutcome::Throttled
        }
    }

    /// `(principal, rejected_total)` snapshot for /metrics.
    pub fn rejection_snapshot(&self) -> Vec<(String, u64)> {
        let buckets = self.buckets.lock().expect("quota bucket mutex");
        let mut v: Vec<(String, u64)> = buckets
            .iter()
            .map(|(k, b)| (k.clone(), b.rejected))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Drop bucket entries that have been at capacity for at least
    /// `EVICT_AFTER`. Called opportunistically by the metrics
    /// handler to bound the map size on long-lived processes.
    pub fn evict_idle(&self) {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("quota bucket mutex");
        buckets.retain(|_, b| {
            let idle = now.saturating_duration_since(b.last_refill);
            idle < EVICT_AFTER || b.tokens < self.burst
        });
    }

    /// Recommended `Retry-After` value (whole seconds) for a 429
    /// response. Caller can use this in the header.
    pub fn retry_after_secs(&self) -> u64 {
        if self.rate_per_sec <= 0.0 {
            return 1;
        }
        (1.0 / self.rate_per_sec).ceil() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_bucket_grants_everything() {
        let q = QuotaBucket::new(0.0, 0.0);
        for _ in 0..1_000 {
            assert_eq!(q.consume("alice"), QuotaOutcome::NotConfigured);
        }
    }

    #[test]
    fn drains_burst_then_throttles() {
        let q = QuotaBucket::new(10.0, 5.0);
        for _ in 0..5 {
            assert_eq!(q.consume("bob"), QuotaOutcome::Granted);
        }
        // 6th call: burst exhausted, no time has passed for refill.
        assert_eq!(q.consume("bob"), QuotaOutcome::Throttled);
    }

    #[test]
    fn refills_at_configured_rate() {
        let q = QuotaBucket::new(100.0, 1.0);
        assert_eq!(q.consume("c"), QuotaOutcome::Granted);
        assert_eq!(q.consume("c"), QuotaOutcome::Throttled);
        std::thread::sleep(Duration::from_millis(20));
        // 100 tokens/sec * 0.020s = 2 tokens; bucket caps at burst=1.
        assert_eq!(q.consume("c"), QuotaOutcome::Granted);
    }

    #[test]
    fn principals_isolate() {
        let q = QuotaBucket::new(10.0, 1.0);
        assert_eq!(q.consume("alice"), QuotaOutcome::Granted);
        assert_eq!(q.consume("alice"), QuotaOutcome::Throttled);
        // bob has his own bucket; alice's exhaustion doesn't affect him.
        assert_eq!(q.consume("bob"), QuotaOutcome::Granted);
    }

    #[test]
    fn rejection_snapshot_counts_throttled_calls() {
        let q = QuotaBucket::new(1.0, 1.0);
        assert_eq!(q.consume("d"), QuotaOutcome::Granted);
        assert_eq!(q.consume("d"), QuotaOutcome::Throttled);
        assert_eq!(q.consume("d"), QuotaOutcome::Throttled);
        let snap = q.rejection_snapshot();
        assert_eq!(snap, vec![("d".to_string(), 2)]);
    }

    #[test]
    fn retry_after_inverse_of_rate() {
        assert_eq!(QuotaBucket::new(1.0, 1.0).retry_after_secs(), 1);
        assert_eq!(QuotaBucket::new(0.5, 1.0).retry_after_secs(), 2);
        assert_eq!(QuotaBucket::new(100.0, 100.0).retry_after_secs(), 1); // ceil(0.01) = 1
    }
}
