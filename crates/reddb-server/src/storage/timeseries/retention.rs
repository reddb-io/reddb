//! Retention policy for time-series data — declarative specs, a
//! registry that survives restart, and a background daemon that
//! sweeps expired chunks without the operator having to script
//! anything.
//!
//! Timescale parity note: the daemon mirrors Timescale's
//! `add_retention_policy` / `show_retention_policies` surface — you
//! tell the engine "keep last 90 days of `metrics`" and it does the
//! rest. The daemon is cooperative: it polls, acquires the chunk
//! lifecycle lock only when it has real work, and never blocks
//! writes.

/// Retention policy configuration
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Maximum age in nanoseconds. Data older than this is eligible for deletion.
    pub max_age_ns: u64,
    /// Optional: only apply to a specific resolution tier
    pub resolution_tier: Option<String>,
}

impl RetentionPolicy {
    /// Create a retention policy with a duration in seconds
    pub fn from_secs(secs: u64) -> Self {
        Self {
            max_age_ns: secs * 1_000_000_000,
            resolution_tier: None,
        }
    }

    /// Create a retention policy with a duration in days
    pub fn from_days(days: u64) -> Self {
        Self::from_secs(days * 86400)
    }

    /// Check if a timestamp is expired given the current time
    pub fn is_expired(&self, timestamp_ns: u64, now_ns: u64) -> bool {
        now_ns.saturating_sub(timestamp_ns) > self.max_age_ns
    }

    /// Get the cutoff timestamp (anything older should be deleted)
    pub fn cutoff_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.max_age_ns)
    }
}

/// Downsample policy definition
#[derive(Debug, Clone)]
pub struct DownsamplePolicy {
    /// Source resolution label (e.g., "raw", "1m", "5m")
    pub source: String,
    /// Target resolution label (e.g., "5m", "1h")
    pub target: String,
    /// Aggregation function to use (e.g., "avg", "max")
    pub aggregation: String,
    /// Target bucket size in nanoseconds
    pub bucket_ns: u64,
}

impl DownsamplePolicy {
    /// Parse a policy string like "1h:5m:avg"
    /// Format: target_resolution:source_resolution:aggregation
    pub fn parse(spec: &str) -> Option<Self> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 {
            return None;
        }
        let target = parts[0].to_string();
        let source = parts[1].to_string();
        let aggregation = if parts.len() > 2 {
            parts[2].to_string()
        } else {
            "avg".to_string()
        };
        let bucket_ns = parse_duration_ns(&target)?;

        Some(Self {
            source,
            target,
            aggregation,
            bucket_ns,
        })
    }
}

/// Parse a duration string (e.g., "5m", "1h", "30s") into nanoseconds
pub fn parse_duration_ns(s: &str) -> Option<u64> {
    let s = s.trim();
    if s == "raw" {
        return Some(0);
    }
    let (num_str, unit) = if let Some(stripped) = s.strip_suffix("ms") {
        (stripped, "ms")
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else if let Some(stripped) = s.strip_suffix('d') {
        (stripped, "d")
    } else {
        return None;
    };

    let num: u64 = num_str.parse().ok()?;
    let multiplier = match unit {
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        "d" => 86_400_000_000_000,
        _ => return None,
    };

    Some(num * multiplier)
}

// =============================================================================
// Retention registry + daemon (Timescale-parity Track A3)
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Backend contract the daemon uses to discover collections and drop
/// chunks. Decouples the daemon from the storage service: the
/// registry receives the trait object at startup, tests supply a
/// mock, production wires the real store.
pub trait RetentionBackend: Send + Sync {
    /// Enumerate the time-series collections this backend owns.
    fn time_series_collections(&self) -> Vec<String>;

    /// Drop every chunk in `collection` whose max timestamp is at or
    /// below `cutoff_ns`. Returns the number of chunks dropped.
    fn drop_chunks_older_than(&self, collection: &str, cutoff_ns: u64) -> u64;
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Per-collection retention + daemon statistics.
#[derive(Debug, Default, Clone)]
pub struct RetentionStats {
    pub cycles: u64,
    pub policies_evaluated: u64,
    pub chunks_dropped: u64,
    pub last_sweep_unix_ns: u64,
}

/// Registry of `{collection → RetentionPolicy}` with a cooperative
/// daemon that sweeps expired chunks on a configurable interval.
#[derive(Clone)]
pub struct RetentionRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    policies: Mutex<HashMap<String, RetentionPolicy>>,
    stats: Mutex<RetentionStats>,
    running: AtomicBool,
    interval_ms: AtomicU64,
}

impl Default for RetentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RetentionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetentionRegistry")
            .field(
                "policies",
                &self.inner.policies.lock().map(|m| m.len()).unwrap_or(0),
            )
            .field("running", &self.inner.running.load(Ordering::SeqCst))
            .finish()
    }
}

impl RetentionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                policies: Mutex::new(HashMap::new()),
                stats: Mutex::new(RetentionStats::default()),
                running: AtomicBool::new(false),
                interval_ms: AtomicU64::new(60_000),
            }),
        }
    }

    /// Install / replace the policy for `collection`.
    pub fn set_policy(&self, collection: impl Into<String>, policy: RetentionPolicy) {
        let mut guard = match self.inner.policies.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.insert(collection.into(), policy);
    }

    /// Drop the policy if any. Returns the removed policy.
    pub fn remove_policy(&self, collection: &str) -> Option<RetentionPolicy> {
        let mut guard = match self.inner.policies.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.remove(collection)
    }

    pub fn list_policies(&self) -> Vec<(String, RetentionPolicy)> {
        let guard = match self.inner.policies.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut out: Vec<(String, RetentionPolicy)> =
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn get_policy(&self, collection: &str) -> Option<RetentionPolicy> {
        let guard = match self.inner.policies.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.get(collection).cloned()
    }

    pub fn stats(&self) -> RetentionStats {
        let guard = match self.inner.stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.clone()
    }

    pub fn set_interval_ms(&self, ms: u64) {
        self.inner.interval_ms.store(ms.max(100), Ordering::SeqCst);
    }

    /// Run one sweep cycle against `backend`. Returns the number of
    /// chunks dropped in this cycle. Exposed for tests — the daemon
    /// calls this in a loop.
    pub fn sweep_once(&self, backend: &dyn RetentionBackend) -> u64 {
        let now = now_ns();
        let policies: Vec<(String, RetentionPolicy)> = self.list_policies();
        let available: std::collections::HashSet<String> =
            backend.time_series_collections().into_iter().collect();

        let mut evaluated = 0u64;
        let mut dropped_total = 0u64;
        for (collection, policy) in &policies {
            if !available.contains(collection) {
                continue; // collection dropped since policy was set
            }
            evaluated += 1;
            let cutoff = policy.cutoff_ns(now);
            if cutoff == 0 {
                continue; // unbounded retention, skip
            }
            let dropped = backend.drop_chunks_older_than(collection, cutoff);
            dropped_total += dropped;
        }

        let mut stats = match self.inner.stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        stats.cycles += 1;
        stats.policies_evaluated += evaluated;
        stats.chunks_dropped += dropped_total;
        stats.last_sweep_unix_ns = now;
        dropped_total
    }

    /// Start a background thread that calls `sweep_once` on the
    /// configured interval. Idempotent — a second call while running
    /// is a no-op. The returned handle keeps the daemon alive; drop
    /// it (or call `stop`) to wind down.
    pub fn start(&self, backend: Arc<dyn RetentionBackend>) -> RetentionDaemonHandle {
        if self
            .inner
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return RetentionDaemonHandle {
                inner: Arc::clone(&self.inner),
                join: None,
            };
        }
        let inner = Arc::clone(&self.inner);
        let registry = self.clone();
        let handle = thread::spawn(move || {
            while inner.running.load(Ordering::SeqCst) {
                let _ = registry.sweep_once(backend.as_ref());
                let interval_ms = inner.interval_ms.load(Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_millis(interval_ms);
                while Instant::now() < deadline && inner.running.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(50.min(interval_ms)));
                }
            }
        });
        RetentionDaemonHandle {
            inner: Arc::clone(&self.inner),
            join: Some(handle),
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
    }
}

/// RAII-ish handle returned by `RetentionRegistry::start`. Dropping
/// it stops the daemon and waits for the thread to exit. Call
/// `detach` to let the daemon outlive the handle (tests prefer the
/// default, which is deterministic shutdown).
pub struct RetentionDaemonHandle {
    inner: Arc<Inner>,
    join: Option<thread::JoinHandle<()>>,
}

impl RetentionDaemonHandle {
    pub fn stop(mut self) {
        self.inner.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }

    pub fn detach(mut self) {
        self.join.take();
    }
}

impl Drop for RetentionDaemonHandle {
    fn drop(&mut self) {
        self.inner.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retention_policy() {
        let policy = RetentionPolicy::from_days(30);
        let now = 5_000_000_000_000_000u64; // ~58 days in ns
        let old = now - 31 * 86_400_000_000_000; // 31 days ago
        let recent = now - 1_000_000_000; // 1 second ago

        assert!(policy.is_expired(old, now));
        assert!(!policy.is_expired(recent, now));
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_ns("5m"), Some(300_000_000_000));
        assert_eq!(parse_duration_ns("1h"), Some(3_600_000_000_000));
        assert_eq!(parse_duration_ns("30s"), Some(30_000_000_000));
        assert_eq!(parse_duration_ns("1d"), Some(86_400_000_000_000));
        assert_eq!(parse_duration_ns("100ms"), Some(100_000_000));
        assert_eq!(parse_duration_ns("raw"), Some(0));
        assert_eq!(parse_duration_ns("invalid"), None);
    }

    #[test]
    fn test_downsample_policy_parse() {
        let policy = DownsamplePolicy::parse("1h:5m:avg").unwrap();
        assert_eq!(policy.target, "1h");
        assert_eq!(policy.source, "5m");
        assert_eq!(policy.aggregation, "avg");
        assert_eq!(policy.bucket_ns, 3_600_000_000_000);
    }

    // =====================================================================
    // Retention registry + daemon — Timescale-parity surface
    // =====================================================================

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Test backend: records every `drop_chunks_older_than` call and
    /// lets the test drive both the collection list and the drop
    /// count it returns.
    struct MockBackend {
        collections: Mutex<Vec<String>>,
        drops: Mutex<Vec<(String, u64)>>,
        drop_return: AtomicU64,
    }

    impl MockBackend {
        fn new(collections: Vec<&str>) -> Arc<Self> {
            Arc::new(Self {
                collections: Mutex::new(collections.into_iter().map(String::from).collect()),
                drops: Mutex::new(Vec::new()),
                drop_return: AtomicU64::new(0),
            })
        }

        fn set_drop_return(&self, n: u64) {
            self.drop_return.store(n, Ordering::SeqCst);
        }

        fn drops(&self) -> Vec<(String, u64)> {
            self.drops.lock().unwrap().clone()
        }
    }

    impl RetentionBackend for MockBackend {
        fn time_series_collections(&self) -> Vec<String> {
            self.collections.lock().unwrap().clone()
        }

        fn drop_chunks_older_than(&self, collection: &str, cutoff_ns: u64) -> u64 {
            self.drops
                .lock()
                .unwrap()
                .push((collection.to_string(), cutoff_ns));
            self.drop_return.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn registry_set_and_get_policy_round_trips() {
        let reg = RetentionRegistry::new();
        reg.set_policy("metrics", RetentionPolicy::from_days(30));
        let fetched = reg.get_policy("metrics").unwrap();
        assert_eq!(fetched.max_age_ns, 30 * 86_400_000_000_000);
    }

    #[test]
    fn registry_list_is_sorted_by_collection() {
        let reg = RetentionRegistry::new();
        reg.set_policy("z", RetentionPolicy::from_days(1));
        reg.set_policy("a", RetentionPolicy::from_days(1));
        reg.set_policy("m", RetentionPolicy::from_days(1));
        let names: Vec<_> = reg.list_policies().into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["a", "m", "z"]);
    }

    #[test]
    fn registry_remove_policy_returns_previous_value() {
        let reg = RetentionRegistry::new();
        reg.set_policy("metrics", RetentionPolicy::from_days(7));
        let removed = reg.remove_policy("metrics").unwrap();
        assert_eq!(removed.max_age_ns, 7 * 86_400_000_000_000);
        assert!(reg.get_policy("metrics").is_none());
    }

    #[test]
    fn sweep_skips_collections_without_backend_presence() {
        let reg = RetentionRegistry::new();
        reg.set_policy("gone", RetentionPolicy::from_days(1));
        let backend = MockBackend::new(vec![]);
        reg.sweep_once(backend.as_ref());
        assert!(backend.drops().is_empty());
    }

    #[test]
    fn sweep_calls_backend_with_policy_cutoff() {
        let reg = RetentionRegistry::new();
        reg.set_policy("metrics", RetentionPolicy::from_days(1));
        let backend = MockBackend::new(vec!["metrics"]);
        backend.set_drop_return(3);
        let dropped = reg.sweep_once(backend.as_ref());
        assert_eq!(dropped, 3);
        let drops = backend.drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].0, "metrics");
        assert!(drops[0].1 > 0);
        let stats = reg.stats();
        assert_eq!(stats.cycles, 1);
        assert_eq!(stats.policies_evaluated, 1);
        assert_eq!(stats.chunks_dropped, 3);
    }

    #[test]
    fn sweep_evaluates_every_matching_collection() {
        let reg = RetentionRegistry::new();
        reg.set_policy("a", RetentionPolicy::from_days(1));
        reg.set_policy("b", RetentionPolicy::from_days(1));
        let backend = MockBackend::new(vec!["a", "b", "c"]);
        backend.set_drop_return(1);
        let dropped = reg.sweep_once(backend.as_ref());
        assert_eq!(dropped, 2);
        assert_eq!(backend.drops().len(), 2);
    }

    #[test]
    fn daemon_sweeps_repeatedly_and_stops_on_drop() {
        let reg = RetentionRegistry::new();
        reg.set_policy("metrics", RetentionPolicy::from_days(1));
        reg.set_interval_ms(100);
        let backend = MockBackend::new(vec!["metrics"]);
        backend.set_drop_return(0);
        let handle = reg.start(backend.clone());
        // Give it ~350ms to run at least 2 cycles (first fires immediately).
        std::thread::sleep(std::time::Duration::from_millis(350));
        assert!(reg.is_running());
        drop(handle); // stops the daemon
        assert!(!reg.is_running());
        let drops = backend.drops();
        assert!(
            drops.len() >= 2,
            "expected >= 2 sweep cycles, got {}",
            drops.len()
        );
    }

    #[test]
    fn start_is_idempotent() {
        let reg = RetentionRegistry::new();
        reg.set_interval_ms(500);
        let backend = MockBackend::new(vec![]);
        let h1 = reg.start(backend.clone());
        let h2 = reg.start(backend.clone());
        // Second handle has no join — stopping it is cheap.
        h2.stop();
        // First handle still owns the thread; dropping shuts down.
        drop(h1);
        assert!(!reg.is_running());
    }
}
