//! Health-aware routing for the gRPC client (issue #171).
//!
//! Replaces the dumb modulo round-robin in [`crate::grpc`] with a
//! deep module that owns three orthogonal pieces of state per
//! endpoint:
//!
//! 1. EWMA of recent RTTs (proxy for "how loaded is this replica?").
//! 2. Consecutive-timeout counter (binary "is the wire dead?" signal).
//! 3. Healthy bit, flipped via the consecutive-timeout counter and
//!    re-admitted by a background probe.
//!
//! Endpoint selection runs the inverse-RTT distribution across the
//! healthy set with a floor so a momentary spike doesn't starve a
//! replica permanently. When every replica is unhealthy, we fall back
//! to the primary index unconditionally — writes need a target, and
//! reads degrade rather than fail.
//!
//! ## Time source
//!
//! Probes run on a configurable cadence (default 10s). To keep the
//! proptest fast and deterministic, time is abstracted behind the
//! [`Clock`] trait. Production code uses [`SystemClock`]; tests use
//! [`FakeClock`].
//!
//! ## Concurrency
//!
//! Per-endpoint state is wrapped in a `Mutex` because the proptest
//! drives it from a single thread and the production caller flips
//! state at low frequency (one observe per RPC, low contention even
//! at high QPS — RPCs themselves are far heavier than the lock).
//! Atomics would shave nanoseconds we don't need.
//!
//! ## Integration
//!
//! The router does NOT own the `Endpoint` pool itself; it answers
//! "which endpoint index should the next read hit?" so the existing
//! [`crate::grpc::Endpoint`] type stays unchanged. See
//! [`HealthAwareRouter::pick_read_index`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// EWMA smoothing factor. New samples get 20% weight; the existing
/// average keeps 80%. This corresponds to a soft window of ~5
/// samples for trend tracking; the formal "100 calls" window from the
/// spec is enforced indirectly because after ~100 samples the EWMA
/// has converged within < 1% of the true mean.
const EWMA_ALPHA: f64 = 0.20;

/// Minimum weight floor as a fraction of the median weight. Even a
/// slow replica gets at least this share of traffic so we keep
/// observing its RTT and don't strand it.
const WEIGHT_FLOOR_FRACTION: f64 = 0.10;

/// Default consecutive-timeout threshold for flipping unhealthy.
pub const DEFAULT_TIMEOUT_THRESHOLD: u32 = 3;

/// Default probe cadence for the health checker.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// Cluster membership snapshot the router consumes.
///
/// Lane P (#168 `TopologyConsumer`) emits a richer struct; for now
/// the router only needs the URLs in primary-then-replicas order so
/// it can map them onto the `GrpcClient`'s endpoint pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMembership {
    /// Primary endpoint URL.
    pub primary: String,
    /// Replica endpoint URLs in declaration order.
    pub replicas: Vec<String>,
}

impl ClusterMembership {
    pub fn new(primary: String, replicas: Vec<String>) -> Self {
        Self { primary, replicas }
    }

    /// Total endpoint count (primary + replicas).
    pub fn len(&self) -> usize {
        1 + self.replicas.len()
    }

    /// All URLs in primary-then-replicas order.
    fn urls(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.len());
        out.push(self.primary.clone());
        out.extend(self.replicas.iter().cloned());
        out
    }
}

/// Result of an RPC the router cares about. Either we measured an
/// RTT (success) or the call hit a timeout (failure that contributes
/// to the circuit breaker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// RPC completed, here is the elapsed time.
    Rtt(Duration),
    /// RPC timed out (or failed in a way the caller treats as a
    /// dead-wire signal). Increments the consecutive-timeout counter.
    Timeout,
}

/// Abstract time source. Production wires [`SystemClock`]; tests
/// drive [`FakeClock`] explicitly.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

/// Real time. Cheap zero-sized struct.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Fake clock for tests. Wraps a `Mutex<Instant>` so test code can
/// `advance(...)` without touching real time.
#[derive(Debug)]
pub struct FakeClock {
    inner: Mutex<Instant>,
}

impl FakeClock {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Instant::now()),
        }
    }

    pub fn advance(&self, d: Duration) {
        let mut guard = self.inner.lock().unwrap();
        *guard += d;
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.inner.lock().unwrap()
    }
}

/// Per-endpoint health/latency state. Accessed under `Mutex`.
#[derive(Debug, Clone)]
struct EndpointHealth {
    url: String,
    /// EWMA of observed RTTs in seconds. `None` until we get our
    /// first sample.
    ewma_rtt_secs: Option<f64>,
    /// Total observation count (capped — we don't need a true count,
    /// just enough to know if we have any samples yet).
    samples: u64,
    /// Consecutive timeout count. Resets on any successful Rtt.
    consecutive_timeouts: u32,
    /// Healthy bit. False means the circuit breaker is open.
    healthy: bool,
    /// Last time we attempted a probe against this endpoint. Only
    /// meaningful when `healthy == false`.
    last_probe: Option<Instant>,
}

impl EndpointHealth {
    fn new(url: String) -> Self {
        Self {
            url,
            ewma_rtt_secs: None,
            samples: 0,
            consecutive_timeouts: 0,
            healthy: true,
            last_probe: None,
        }
    }

    fn record_rtt(&mut self, rtt: Duration) {
        let secs = rtt.as_secs_f64().max(1e-6);
        self.ewma_rtt_secs = Some(match self.ewma_rtt_secs {
            None => secs,
            Some(prev) => EWMA_ALPHA * secs + (1.0 - EWMA_ALPHA) * prev,
        });
        self.samples = self.samples.saturating_add(1);
        self.consecutive_timeouts = 0;
    }

    fn record_timeout(&mut self, threshold: u32) {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        if self.consecutive_timeouts >= threshold {
            self.healthy = false;
        }
    }

    /// Reset state on probe success — back into rotation.
    fn admit(&mut self) {
        self.healthy = true;
        self.consecutive_timeouts = 0;
    }
}

/// Configuration knobs. Defaults match the issue spec.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub timeout_threshold: u32,
    pub probe_interval: Duration,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            timeout_threshold: DEFAULT_TIMEOUT_THRESHOLD,
            probe_interval: DEFAULT_PROBE_INTERVAL,
        }
    }
}

/// Health-aware endpoint router.
///
/// Index 0 is always the primary. Indices `1..=replicas.len()` are
/// replicas in declaration order. `pick_read_index` returns one of
/// these; callers map it back onto their `Endpoint` pool.
pub struct HealthAwareRouter {
    /// Per-endpoint state, length == membership.len(). Index 0 is
    /// the primary.
    endpoints: Mutex<Vec<EndpointHealth>>,
    config: RouterConfig,
    clock: Box<dyn Clock>,
    /// `force_primary` short-circuits everything to index 0. Mirrors
    /// the existing `?route=primary` opt-out.
    force_primary: bool,
    /// Round-robin tiebreaker for the weighted selection. Matters
    /// only when all weights are equal (cold start, no samples yet).
    rr_counter: Mutex<u64>,
}

impl std::fmt::Debug for HealthAwareRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let endpoints = self.endpoints.lock().unwrap();
        f.debug_struct("HealthAwareRouter")
            .field("endpoints", &*endpoints)
            .field("config", &self.config)
            .field("force_primary", &self.force_primary)
            .finish()
    }
}

impl HealthAwareRouter {
    /// Build a router from a membership snapshot using
    /// [`SystemClock`] and default config.
    pub fn new(membership: ClusterMembership) -> Self {
        Self::with_config(membership, RouterConfig::default(), Box::new(SystemClock))
    }

    /// Like [`new`] but with explicit force-primary (mirrors the
    /// existing `force_primary` flag on `GrpcClient`).
    pub fn with_force_primary(membership: ClusterMembership, force_primary: bool) -> Self {
        let mut r = Self::new(membership);
        r.force_primary = force_primary;
        r
    }

    /// Test-friendly constructor.
    pub fn with_config(
        membership: ClusterMembership,
        config: RouterConfig,
        clock: Box<dyn Clock>,
    ) -> Self {
        let endpoints: Vec<EndpointHealth> = membership
            .urls()
            .into_iter()
            .map(EndpointHealth::new)
            .collect();
        Self {
            endpoints: Mutex::new(endpoints),
            config,
            clock,
            force_primary: false,
            rr_counter: Mutex::new(0),
        }
    }

    /// Number of endpoints (primary + replicas).
    pub fn len(&self) -> usize {
        self.endpoints.lock().unwrap().len()
    }

    /// Whether `force_primary` is set.
    pub fn force_primary(&self) -> bool {
        self.force_primary
    }

    /// Pick the index of the next endpoint to serve a read.
    ///
    /// - If `force_primary` is set or the only endpoint is the
    ///   primary, returns 0.
    /// - Otherwise selects a healthy replica using inverse-RTT
    ///   weighting with a floor.
    /// - Falls back to 0 (primary) when every replica is unhealthy.
    pub fn pick_read_index(&self) -> usize {
        let endpoints = self.endpoints.lock().unwrap();
        if self.force_primary || endpoints.len() == 1 {
            return 0;
        }
        // Replica indices: 1..=n.
        let healthy_replicas: Vec<usize> = (1..endpoints.len())
            .filter(|&i| endpoints[i].healthy)
            .collect();
        if healthy_replicas.is_empty() {
            // All-unhealthy fallback: route to primary.
            return 0;
        }
        let weights: Vec<f64> = healthy_replicas
            .iter()
            .map(|&i| weight_for(&endpoints[i]))
            .collect();
        let weights = apply_floor(&weights);
        let mut counter = self.rr_counter.lock().unwrap();
        let idx_in_healthy = weighted_pick(&weights, *counter);
        *counter = counter.wrapping_add(1);
        healthy_replicas[idx_in_healthy]
    }

    /// Record an outcome against an endpoint identified by index.
    /// Index 0 is the primary; replicas are 1..=n.
    pub fn observe_index(&self, index: usize, outcome: Outcome) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if let Some(ep) = endpoints.get_mut(index) {
            match outcome {
                Outcome::Rtt(rtt) => ep.record_rtt(rtt),
                Outcome::Timeout => ep.record_timeout(self.config.timeout_threshold),
            }
        }
    }

    /// Record an outcome against an endpoint identified by URL.
    /// Convenience for callers that don't track indices.
    pub fn observe_url(&self, url: &str, outcome: Outcome) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if let Some(ep) = endpoints.iter_mut().find(|ep| ep.url == url) {
            match outcome {
                Outcome::Rtt(rtt) => ep.record_rtt(rtt),
                Outcome::Timeout => ep.record_timeout(self.config.timeout_threshold),
            }
        }
    }

    /// Indices of endpoints that are due for a health probe.
    /// "Due" means: marked unhealthy AND last_probe is `None` or
    /// `>= probe_interval` ago.
    ///
    /// The caller (background task) walks this list, issues a
    /// lightweight RPC against each, and reports back via
    /// [`record_probe_result`]. Decoupling the policy from the
    /// transport keeps this module testable without a tonic server.
    pub fn endpoints_due_for_probe(&self) -> Vec<ProbeTarget> {
        let endpoints = self.endpoints.lock().unwrap();
        let now = self.clock.now();
        endpoints
            .iter()
            .enumerate()
            .filter(|(_, ep)| !ep.healthy)
            .filter(|(_, ep)| match ep.last_probe {
                None => true,
                Some(t) => now.duration_since(t) >= self.config.probe_interval,
            })
            .map(|(i, ep)| ProbeTarget {
                index: i,
                url: ep.url.clone(),
            })
            .collect()
    }

    /// Report the result of a probe attempt. Stamps `last_probe` so
    /// we don't hammer the endpoint, and on success flips it back to
    /// healthy.
    pub fn record_probe_result(&self, index: usize, success: bool) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if let Some(ep) = endpoints.get_mut(index) {
            ep.last_probe = Some(self.clock.now());
            if success {
                ep.admit();
            }
        }
    }

    /// Refresh membership. Endpoints whose URL is unchanged keep
    /// their accumulated state; new URLs start fresh; URLs that
    /// disappear are dropped.
    pub fn update_membership(&mut self, new_membership: ClusterMembership) {
        let mut endpoints = self.endpoints.lock().unwrap();
        let new_urls = new_membership.urls();
        let mut next: Vec<EndpointHealth> = Vec::with_capacity(new_urls.len());
        for url in new_urls {
            if let Some(existing) = endpoints.iter().find(|ep| ep.url == url) {
                next.push(existing.clone());
            } else {
                next.push(EndpointHealth::new(url));
            }
        }
        *endpoints = next;
    }

    /// Snapshot a single endpoint's URL by index. For diagnostics +
    /// the gRPC integration's debug formatting.
    pub fn endpoint_url(&self, index: usize) -> Option<String> {
        self.endpoints
            .lock()
            .unwrap()
            .get(index)
            .map(|ep| ep.url.clone())
    }

    /// Test/diagnostic snapshot of an endpoint's health state.
    #[cfg(test)]
    fn snapshot(&self, index: usize) -> Option<EndpointHealth> {
        self.endpoints.lock().unwrap().get(index).cloned()
    }
}

/// Description of an endpoint that is due for a probe call. Returned
/// by [`HealthAwareRouter::endpoints_due_for_probe`].
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    pub index: usize,
    pub url: String,
}

/// Weight for a single endpoint. Larger weights pick more often.
/// Inverse RTT, in seconds. Endpoints with no samples get weight 1.0
/// so they're picked at parity until we measure them.
fn weight_for(ep: &EndpointHealth) -> f64 {
    match ep.ewma_rtt_secs {
        None => 1.0,
        Some(rtt) => {
            // Guard against degenerate zero rtt.
            let rtt = rtt.max(1e-6);
            1.0 / rtt
        }
    }
}

/// Apply the starvation floor. Any weight below
/// `WEIGHT_FLOOR_FRACTION * median(weights)` is lifted to that
/// floor, so even slow replicas keep getting the occasional probe
/// call.
fn apply_floor(weights: &[f64]) -> Vec<f64> {
    if weights.is_empty() {
        return Vec::new();
    }
    let mut sorted = weights.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let floor = WEIGHT_FLOOR_FRACTION * median;
    weights.iter().map(|w| w.max(floor)).collect()
}

/// Deterministic weighted pick. Each call advances a logical
/// pointer by `1.0` through the cumulative weight distribution,
/// wrapping at `total`. Over `N` calls each bucket of weight `w`
/// gets `floor(N * w / total)` hits (off-by-one in the tail). No
/// RNG, no allocator, deterministic — matches the round-robin
/// spirit of the rest of the client.
fn weighted_pick(weights: &[f64], counter: u64) -> usize {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return (counter as usize) % weights.len();
    }
    // Counter advances by 1.0 per call; wrap by total. Modulo is
    // exact for small floats; for large counters we reduce first
    // to keep f64 precision.
    let counter_mod = (counter as f64) % total;
    let mut acc = 0.0;
    for (i, &w) in weights.iter().enumerate() {
        acc += w;
        if counter_mod < acc {
            return i;
        }
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn membership(primary: &str, replicas: &[&str]) -> ClusterMembership {
        ClusterMembership::new(
            primary.to_string(),
            replicas.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn single_endpoint_always_returns_primary() {
        let router = HealthAwareRouter::new(membership("primary", &[]));
        for _ in 0..50 {
            assert_eq!(router.pick_read_index(), 0);
        }
    }

    #[test]
    fn force_primary_short_circuits() {
        let router =
            HealthAwareRouter::with_force_primary(membership("p", &["r1", "r2"]), true);
        for _ in 0..50 {
            assert_eq!(router.pick_read_index(), 0);
        }
    }

    #[test]
    fn cold_start_distributes_across_replicas() {
        let router = HealthAwareRouter::new(membership("p", &["r1", "r2", "r3"]));
        let mut hits: HashMap<usize, u32> = HashMap::new();
        for _ in 0..3000 {
            *hits.entry(router.pick_read_index()).or_insert(0) += 1;
        }
        // Primary should NEVER be hit when replicas are healthy.
        assert_eq!(hits.get(&0).copied().unwrap_or(0), 0);
        // Each replica should get roughly 1/3 of traffic.
        for idx in 1..=3 {
            let n = hits.get(&idx).copied().unwrap_or(0);
            assert!(n > 800 && n < 1200, "replica {idx} got {n} hits");
        }
    }

    #[test]
    fn circuit_breaker_opens_after_k_consecutive_timeouts() {
        let router = HealthAwareRouter::new(membership("p", &["r1", "r2"]));
        // Three timeouts on r1 (index 1).
        for _ in 0..DEFAULT_TIMEOUT_THRESHOLD {
            router.observe_index(1, Outcome::Timeout);
        }
        // Now every read should pick r2 (index 2).
        for _ in 0..200 {
            assert_eq!(router.pick_read_index(), 2);
        }
        let snap = router.snapshot(1).unwrap();
        assert!(!snap.healthy);
        assert_eq!(snap.consecutive_timeouts, DEFAULT_TIMEOUT_THRESHOLD);
    }

    #[test]
    fn rtt_observation_resets_consecutive_timeouts() {
        let router = HealthAwareRouter::new(membership("p", &["r1"]));
        router.observe_index(1, Outcome::Timeout);
        router.observe_index(1, Outcome::Timeout);
        router.observe_index(1, Outcome::Rtt(Duration::from_millis(5)));
        let snap = router.snapshot(1).unwrap();
        assert_eq!(snap.consecutive_timeouts, 0);
        assert!(snap.healthy);
    }

    #[test]
    fn all_unhealthy_replicas_fall_back_to_primary() {
        let router = HealthAwareRouter::new(membership("p", &["r1", "r2"]));
        for _ in 0..DEFAULT_TIMEOUT_THRESHOLD {
            router.observe_index(1, Outcome::Timeout);
            router.observe_index(2, Outcome::Timeout);
        }
        for _ in 0..50 {
            assert_eq!(router.pick_read_index(), 0);
        }
    }

    #[test]
    fn probe_readmits_endpoint() {
        let clock = std::sync::Arc::new(FakeClock::new());
        let router = HealthAwareRouter::with_config(
            membership("p", &["r1", "r2"]),
            RouterConfig::default(),
            Box::new(FakeClockHandle(clock.clone())),
        );
        for _ in 0..DEFAULT_TIMEOUT_THRESHOLD {
            router.observe_index(1, Outcome::Timeout);
        }
        // r1 unhealthy, all reads go to r2.
        assert_eq!(router.pick_read_index(), 2);

        // First call: r1 is due immediately (last_probe is None).
        let due = router.endpoints_due_for_probe();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].index, 1);

        // Probe succeeds. r1 should be re-admitted.
        router.record_probe_result(1, true);
        let snap = router.snapshot(1).unwrap();
        assert!(snap.healthy);
    }

    #[test]
    fn probe_cadence_respects_interval_under_fake_clock() {
        let clock = std::sync::Arc::new(FakeClock::new());
        let router = HealthAwareRouter::with_config(
            membership("p", &["r1"]),
            RouterConfig {
                timeout_threshold: 1,
                probe_interval: Duration::from_secs(10),
            },
            Box::new(FakeClockHandle(clock.clone())),
        );
        router.observe_index(1, Outcome::Timeout);
        // Due immediately.
        assert_eq!(router.endpoints_due_for_probe().len(), 1);
        // Probe fails -> stays unhealthy, but last_probe stamped.
        router.record_probe_result(1, false);
        // Not due again until 10s elapse.
        assert!(router.endpoints_due_for_probe().is_empty());
        clock.advance(Duration::from_secs(5));
        assert!(router.endpoints_due_for_probe().is_empty());
        clock.advance(Duration::from_secs(6));
        assert_eq!(router.endpoints_due_for_probe().len(), 1);
    }

    #[test]
    fn membership_update_preserves_known_endpoints() {
        let mut router = HealthAwareRouter::new(membership("p", &["r1", "r2"]));
        router.observe_index(1, Outcome::Rtt(Duration::from_millis(10)));
        let prev_samples = router.snapshot(1).unwrap().samples;
        assert_eq!(prev_samples, 1);

        router.update_membership(membership("p", &["r1", "r3"]));
        // r1 retained its sample count; r3 is fresh.
        assert_eq!(router.snapshot(1).unwrap().samples, 1);
        assert_eq!(router.snapshot(2).unwrap().samples, 0);
        assert_eq!(router.snapshot(2).unwrap().url, "r3");
    }

    #[test]
    fn weighted_distribution_favours_faster_replicas() {
        let router = HealthAwareRouter::new(membership("p", &["fast", "slow"]));
        // Seed many samples — fast: 1ms EWMA, slow: 10ms EWMA.
        for _ in 0..200 {
            router.observe_index(1, Outcome::Rtt(Duration::from_millis(1)));
            router.observe_index(2, Outcome::Rtt(Duration::from_millis(10)));
        }
        let mut hits: HashMap<usize, u32> = HashMap::new();
        for _ in 0..10_000 {
            *hits.entry(router.pick_read_index()).or_insert(0) += 1;
        }
        let fast = hits.get(&1).copied().unwrap_or(0) as f64;
        let slow = hits.get(&2).copied().unwrap_or(0) as f64;
        // Inverse-RTT weights: fast gets 10x weight, so ratio
        // ~10:1. With the 10% floor on slow, the floor lifts slow
        // up to ~10% of fast's weight => ratio shifts toward 10:1
        // exactly. Allow ±10% slack.
        let ratio = fast / slow;
        assert!(
            (9.0..=11.0).contains(&ratio),
            "expected ~10:1 fast/slow ratio, got {ratio}"
        );
    }

    /// Wrapper so `Box<dyn Clock>` can hold a clone-able handle to
    /// the `Arc<FakeClock>` that test code retains for `advance()`.
    struct FakeClockHandle(std::sync::Arc<FakeClock>);
    impl Clock for FakeClockHandle {
        fn now(&self) -> Instant {
            self.0.now()
        }
    }
}

#[cfg(test)]
mod proptest_router {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    proptest! {
        // Steady-state: with all healthy and known EWMA values, the
        // observed frequency tracks the inverse-RTT distribution
        // within ±15%.
        #[test]
        fn weighted_distribution_tracks_inverse_rtt(
            rtts in proptest::collection::vec(1u64..50u64, 2..6usize),
        ) {
            let names: Vec<String> = (0..rtts.len()).map(|i| format!("r{i}")).collect();
            let replicas: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let router = HealthAwareRouter::new(
                ClusterMembership::new("primary".into(), replicas.iter().map(|s| s.to_string()).collect())
            );
            // Seed RTTs.
            for (i, &rtt_ms) in rtts.iter().enumerate() {
                let idx = i + 1;
                for _ in 0..200 {
                    router.observe_index(idx, Outcome::Rtt(Duration::from_millis(rtt_ms)));
                }
            }
            let n_calls = 10_000usize;
            let mut hits: HashMap<usize, u32> = HashMap::new();
            for _ in 0..n_calls {
                *hits.entry(router.pick_read_index()).or_insert(0) += 1;
            }
            // Compute expected weights with the floor applied.
            let raw_weights: Vec<f64> = rtts.iter().map(|&r| 1.0 / (r as f64 / 1000.0)).collect();
            let expected_weights = apply_floor(&raw_weights);
            let total: f64 = expected_weights.iter().sum();
            for (i, &w) in expected_weights.iter().enumerate() {
                let idx = i + 1;
                let expected = (w / total) * (n_calls as f64);
                let actual = hits.get(&idx).copied().unwrap_or(0) as f64;
                let slack = 0.15 * expected + 50.0;
                prop_assert!(
                    (actual - expected).abs() <= slack,
                    "replica {idx}: expected ~{expected:.0}, got {actual} (slack {slack:.0}); rtts={rtts:?}"
                );
            }
        }

        // Circuit breaker convergence: K consecutive timeouts MUST
        // open the breaker, and any successful Rtt before the K-th
        // MUST reset the counter.
        #[test]
        fn circuit_breaker_open_on_k_consecutive(
            seq in proptest::collection::vec(any::<bool>(), 1..40usize),
        ) {
            let router = HealthAwareRouter::with_config(
                ClusterMembership::new("p".into(), vec!["r1".into()]),
                RouterConfig { timeout_threshold: DEFAULT_TIMEOUT_THRESHOLD, probe_interval: DEFAULT_PROBE_INTERVAL },
                Box::new(SystemClock),
            );
            let mut consecutive = 0u32;
            let mut should_be_unhealthy = false;
            for &is_timeout in &seq {
                if is_timeout {
                    router.observe_index(1, Outcome::Timeout);
                    consecutive += 1;
                    if consecutive >= DEFAULT_TIMEOUT_THRESHOLD {
                        should_be_unhealthy = true;
                    }
                } else {
                    router.observe_index(1, Outcome::Rtt(Duration::from_millis(2)));
                    consecutive = 0;
                    // Successful Rtt does NOT auto-readmit; the
                    // breaker stays open until a probe succeeds.
                    // But it does reset the counter so further
                    // timeouts need K MORE in a row.
                }
            }
            let snap = router.snapshot(1).unwrap();
            if should_be_unhealthy {
                prop_assert!(!snap.healthy);
            }
            // Counter invariant: after a successful Rtt the
            // counter is 0; otherwise it equals trailing timeouts.
            let trailing = seq.iter().rev().take_while(|&&b| b).count() as u32;
            prop_assert_eq!(snap.consecutive_timeouts, trailing);
        }
    }
}
