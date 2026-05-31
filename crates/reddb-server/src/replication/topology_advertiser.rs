//! Server-side `TopologyAdvertiser` (issue #167).
//!
//! Produces the canonical `Topology` payload (defined in
//! `reddb-wire::topology`) from the live primary replica registry,
//! gated by the `cluster:topology:read` capability per ADR 0008.
//!
//! ## Auth gate
//!
//! ADR 0008 §1–§3 specifies one capability — `cluster:topology:read` —
//! evaluated against the principal that opened the connection.
//! Authenticated principals get the capability by default, anonymous
//! callers do not. The advertiser collapses to a *primary-only* payload
//! when the gate denies (ADR 0008 §3) — a `Topology { primary, replicas: [] }`
//! that still carries the write-path endpoint so unauthenticated
//! bootstrap keeps working without leaking the replica fleet.
//!
//! ## Health + lag
//!
//! * `healthy = (now_ms - last_seen_at_unix_ms) <= replica_timeout_ms`
//! * `lag_ms = primary.current_lsn - replica.last_applied_lsn` mapped
//!   to milliseconds via the recent commit-rate estimate. When the
//!   estimate cannot be produced (no commit history yet), the field
//!   reports `u32::MAX` per the issue spec — the consumer side treats
//!   that as "lag unknown, fall back to URI-only routing for this
//!   replica".
//!
//! ## Module shape
//!
//! `TopologyAdvertiser` is the deep module: callers pass in the
//! replica snapshot, an auth context, the current epoch, the primary
//! endpoint, and a configuration knob for lag/health. It returns a
//! `Topology` ready for the wire encoder. The auth predicate is
//! extracted as `TopologyAuthGate` so it can be unit-tested in
//! isolation without booting the rest of the advertiser.

use crate::auth::middleware::AuthResult;
use crate::replication::primary::ReplicaState;
use reddb_wire::topology::{Endpoint, ReplicaInfo, Topology};

/// Default replica heartbeat timeout used when an operator hasn't
/// configured one explicitly. Matches the order of the `poll_interval_ms`
/// default in `ReplicationConfig` (100 ms) multiplied by a generous
/// fudge factor — five seconds without an ack flips a replica to
/// `healthy: false`. Operators tune this via `LagConfig`.
pub const DEFAULT_REPLICA_TIMEOUT_MS: u128 = 5_000;

/// Capability name from ADR 0008 §1.
///
/// Kept as a constant rather than scattered string literals so a
/// future capability-engine integration can grep for one symbol.
pub const TOPOLOGY_READ_CAPABILITY: &str = "cluster:topology:read";

// ---------------------------------------------------------------
// TopologyAuthGate
// ---------------------------------------------------------------

/// Predicate over the caller's auth context — answers "does this
/// principal have `cluster:topology:read`?".
///
/// Extracted so the gate can be unit-tested in isolation, the way
/// ADR 0008 §1 wants ("one capability, one check, one place to
/// grep"). The advertiser composes this gate, never reimplements it.
///
/// Until the capability engine lands, the policy is the one approved
/// in ADR 0008 §2: every authenticated principal carries the
/// capability by default; anonymous and denied callers do not.
pub struct TopologyAuthGate;

impl TopologyAuthGate {
    /// `true` if the principal carries `cluster:topology:read` and
    /// should receive the full topology. `false` collapses the
    /// advertiser output to primary-only per ADR 0008 §3.
    pub fn allows(auth: &AuthResult) -> bool {
        match auth {
            AuthResult::Authenticated { .. } => true,
            AuthResult::Anonymous => false,
            AuthResult::Denied(_) => false,
        }
    }
}

// ---------------------------------------------------------------
// LagConfig
// ---------------------------------------------------------------

/// Knobs for the lag/health computation. Kept as a small struct so
/// the call sites (gRPC `topology` RPC, RedWire HelloAck builder)
/// thread the same defaults without each one redeclaring constants.
#[derive(Debug, Clone, Copy)]
pub struct LagConfig {
    /// Replica heartbeats older than this flip `healthy` to `false`.
    pub replica_timeout_ms: u128,
    /// Recent-progress estimate: how many WAL records the cluster
    /// applies per millisecond on average. `None` → lag conversion
    /// degrades gracefully to `u32::MAX` (issue spec: "if not
    /// estimable").
    pub records_per_ms: Option<f64>,
    /// `now` in unix milliseconds. Threaded explicitly so tests can
    /// pin a deterministic clock without reaching for a global.
    pub now_unix_ms: u128,
}

impl LagConfig {
    /// Sensible default for production callers — `replica_timeout`
    /// at the module default, no progress estimate (so lag reports
    /// `u32::MAX`), `now` from the system clock.
    pub fn from_now() -> Self {
        Self {
            replica_timeout_ms: DEFAULT_REPLICA_TIMEOUT_MS,
            records_per_ms: None,
            now_unix_ms: crate::utils::now_unix_millis() as u128,
        }
    }
}

// ---------------------------------------------------------------
// TopologyAdvertiser
// ---------------------------------------------------------------

/// Server-side advertiser. Zero-sized — all state is threaded
/// through `advertise()`'s arguments so callers control the
/// snapshot semantics.
pub struct TopologyAdvertiser;

impl TopologyAdvertiser {
    /// Build the canonical `Topology` payload for the given caller.
    ///
    /// * `replicas` — snapshot of the primary's replica registry
    ///   (`PrimaryReplication::replica_snapshots()`).
    /// * `auth` — caller's resolved auth context. Drives the
    ///   capability gate (ADR 0008 §1).
    /// * `epoch` — registry-change epoch; clients use this to detect
    ///   stale advertisements.
    /// * `primary_endpoint` — what the primary advertises about
    ///   itself. Always returned regardless of auth (ADR 0008 §3).
    /// * `primary_current_lsn` — primary's current WAL LSN, used as
    ///   the reference for the per-replica lag computation.
    /// * `lag` — knobs for the lag/health translation.
    pub fn advertise(
        replicas: &[ReplicaState],
        auth: &AuthResult,
        epoch: u64,
        primary_endpoint: Endpoint,
        primary_current_lsn: u64,
        lag: &LagConfig,
    ) -> Topology {
        // ADR 0008 §3: anonymous / denied → primary-only.
        if !TopologyAuthGate::allows(auth) {
            return Topology {
                epoch,
                primary: primary_endpoint,
                replicas: Vec::new(),
            };
        }

        let infos = replicas
            .iter()
            .map(|r| replica_to_info(r, primary_current_lsn, lag))
            .collect();
        Topology {
            epoch,
            primary: primary_endpoint,
            replicas: infos,
        }
    }
}

// ---------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------

fn replica_to_info(state: &ReplicaState, primary_lsn: u64, lag: &LagConfig) -> ReplicaInfo {
    let healthy = is_healthy(state, lag);
    let lag_ms = compute_lag_ms(state, primary_lsn, lag);
    ReplicaInfo {
        // The replica `id` doubles as the gRPC address handed in at
        // registration time. Keeping the same field lets the consumer
        // dial the replica directly without a second lookup table.
        addr: state.id.clone(),
        region: state
            .region
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        healthy,
        lag_ms,
        last_applied_lsn: state.last_acked_lsn,
        // Surface the replica's re-bootstrap state so the consumer's
        // routing table can exclude it from causal reads (issue #837).
        rebootstrapping: state.rebootstrapping,
    }
}

fn is_healthy(state: &ReplicaState, lag: &LagConfig) -> bool {
    let last_seen = state.last_seen_at_unix_ms;
    if lag.now_unix_ms < last_seen {
        // Clock skew — treat the most recent ack as fresh rather
        // than flag a healthy replica as stale.
        return true;
    }
    (lag.now_unix_ms - last_seen) <= lag.replica_timeout_ms
}

fn compute_lag_ms(state: &ReplicaState, primary_lsn: u64, lag: &LagConfig) -> u32 {
    let lag_records = primary_lsn.saturating_sub(state.last_acked_lsn);
    if lag_records == 0 {
        return 0;
    }
    let Some(rate) = lag.records_per_ms else {
        // Issue spec: not estimable → u32::MAX.
        return u32::MAX;
    };
    if rate <= 0.0 || !rate.is_finite() {
        return u32::MAX;
    }
    let ms = (lag_records as f64) / rate;
    if !ms.is_finite() || ms < 0.0 {
        return u32::MAX;
    }
    if ms >= u32::MAX as f64 {
        return u32::MAX;
    }
    ms.round() as u32
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::middleware::AuthSource;
    use crate::auth::Role;

    // -----------------------------------------------------------
    // Auth-context fixtures: the four canonical principals from
    // ADR 0008 (anonymous, tenant, operator, admin). The gate is
    // role-blind today — ADR §1 says one capability, not a role
    // ladder — so all three authenticated principals collapse to
    // "has capability". We still keep the fixtures distinct so
    // future capability-engine integration can override per-role
    // behaviour without rewriting the test matrix.
    // -----------------------------------------------------------

    fn anonymous() -> AuthResult {
        AuthResult::Anonymous
    }

    fn tenant() -> AuthResult {
        AuthResult::Authenticated {
            username: "tenant-alice".into(),
            role: Role::Read,
            source: AuthSource::Password,
        }
    }

    fn operator() -> AuthResult {
        AuthResult::Authenticated {
            username: "operator-bob".into(),
            role: Role::Write,
            source: AuthSource::Password,
        }
    }

    fn admin() -> AuthResult {
        AuthResult::Authenticated {
            username: "admin-root".into(),
            role: Role::Admin,
            source: AuthSource::Password,
        }
    }

    fn primary_ep() -> Endpoint {
        Endpoint {
            addr: "primary.example.com:5050".into(),
            region: "us-east-1".into(),
        }
    }

    fn replica(id: &str, region: Option<&str>, last_seen_offset_ms: i128) -> ReplicaState {
        // last_seen_offset_ms is measured against `lag_now_ms()` —
        // negative values mean "older than now", positive means
        // "in the future" (clock skew test).
        let now = lag_now_ms();
        let last_seen = (now as i128 + last_seen_offset_ms).max(0) as u128;
        ReplicaState {
            id: id.to_string(),
            last_acked_lsn: 100,
            last_sent_lsn: 100,
            last_durable_lsn: 100,
            apply_error_count: 0,
            divergence_count: 0,
            connected_at_unix_ms: now,
            last_seen_at_unix_ms: last_seen,
            region: region.map(String::from),
            rebootstrapping: false,
        }
    }

    fn lag_now_ms() -> u128 {
        // Pinned clock so health computations are deterministic.
        1_700_000_000_000
    }

    fn lag_default() -> LagConfig {
        LagConfig {
            replica_timeout_ms: DEFAULT_REPLICA_TIMEOUT_MS,
            records_per_ms: None,
            now_unix_ms: lag_now_ms(),
        }
    }

    // -----------------------------------------------------------
    // Auth gate (predicate-only, separate from the advertiser).
    // -----------------------------------------------------------

    #[test]
    fn topology_advertiser_gate_allows_authenticated() {
        assert!(TopologyAuthGate::allows(&tenant()));
        assert!(TopologyAuthGate::allows(&operator()));
        assert!(TopologyAuthGate::allows(&admin()));
    }

    #[test]
    fn topology_advertiser_gate_blocks_anonymous_and_denied() {
        assert!(!TopologyAuthGate::allows(&anonymous()));
        assert!(!TopologyAuthGate::allows(&AuthResult::Denied(
            "nope".into()
        )));
    }

    // -----------------------------------------------------------
    // Auth × registry-shape matrix (issue spec §"Tests" first
    // bullet). 4 principals × 3 shapes (empty, 1 replica,
    // multi-region).
    // -----------------------------------------------------------

    fn shape_empty() -> Vec<ReplicaState> {
        Vec::new()
    }

    fn shape_one() -> Vec<ReplicaState> {
        vec![replica("replica-a:5050", Some("us-east-1"), -100)]
    }

    fn shape_multi_region() -> Vec<ReplicaState> {
        vec![
            replica("replica-a:5050", Some("us-east-1"), -100),
            replica("replica-b:5050", Some("us-west-2"), -200),
            replica("replica-c:5050", Some("eu-central-1"), -300),
        ]
    }

    #[test]
    fn topology_advertiser_anonymous_gets_primary_only() {
        // ADR 0008 §3: every shape collapses to primary-only for
        // the unauthenticated caller — including the multi-region
        // case where the disclosure-leak risk is highest.
        for shape in [shape_empty(), shape_one(), shape_multi_region()] {
            let topo = TopologyAdvertiser::advertise(
                &shape,
                &anonymous(),
                42,
                primary_ep(),
                500,
                &lag_default(),
            );
            assert_eq!(topo.epoch, 42);
            assert_eq!(topo.primary, primary_ep());
            assert!(
                topo.replicas.is_empty(),
                "anonymous must never see replicas, got {:?}",
                topo.replicas
            );
        }
    }

    #[test]
    fn topology_advertiser_authenticated_gets_full_list() {
        // All three authenticated principals see every replica —
        // the gate is capability-driven, not role-driven (ADR §1).
        for ctx in [tenant(), operator(), admin()] {
            let topo = TopologyAdvertiser::advertise(
                &shape_multi_region(),
                &ctx,
                7,
                primary_ep(),
                500,
                &lag_default(),
            );
            assert_eq!(topo.epoch, 7);
            assert_eq!(topo.replicas.len(), 3);
            let regions: Vec<&str> = topo.replicas.iter().map(|r| r.region.as_str()).collect();
            assert!(regions.contains(&"us-east-1"));
            assert!(regions.contains(&"us-west-2"));
            assert!(regions.contains(&"eu-central-1"));
        }
    }

    #[test]
    fn topology_advertiser_authenticated_empty_registry_returns_no_replicas() {
        let topo = TopologyAdvertiser::advertise(
            &shape_empty(),
            &admin(),
            1,
            primary_ep(),
            0,
            &lag_default(),
        );
        assert!(topo.replicas.is_empty());
        assert_eq!(topo.primary, primary_ep());
    }

    #[test]
    fn topology_advertiser_denied_collapses_to_primary_only() {
        let topo = TopologyAdvertiser::advertise(
            &shape_multi_region(),
            &AuthResult::Denied("invalid token".into()),
            9,
            primary_ep(),
            500,
            &lag_default(),
        );
        assert!(topo.replicas.is_empty());
    }

    // -----------------------------------------------------------
    // Health: ack window flips healthy.
    // -----------------------------------------------------------

    #[test]
    fn topology_advertiser_recent_ack_is_healthy() {
        let mut shape = shape_one();
        shape[0].last_seen_at_unix_ms = lag_now_ms() - 100;
        let topo =
            TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 100, &lag_default());
        assert!(topo.replicas[0].healthy);
    }

    #[test]
    fn topology_advertiser_stale_ack_is_unhealthy() {
        let mut shape = shape_one();
        // Older than the timeout — flip to unhealthy.
        shape[0].last_seen_at_unix_ms = lag_now_ms() - DEFAULT_REPLICA_TIMEOUT_MS - 1;
        let topo =
            TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 100, &lag_default());
        assert!(!topo.replicas[0].healthy);
    }

    // -----------------------------------------------------------
    // Lag: degrades gracefully to u32::MAX when no estimate.
    // -----------------------------------------------------------

    #[test]
    fn topology_advertiser_lag_unknown_reports_u32_max() {
        let mut shape = shape_one();
        shape[0].last_acked_lsn = 50;
        let topo =
            TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 500, &lag_default());
        assert_eq!(topo.replicas[0].lag_ms, u32::MAX);
    }

    #[test]
    fn topology_advertiser_lag_zero_when_replica_caught_up() {
        let mut shape = shape_one();
        shape[0].last_acked_lsn = 500;
        let topo =
            TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 500, &lag_default());
        assert_eq!(topo.replicas[0].lag_ms, 0);
    }

    #[test]
    fn topology_advertiser_lag_uses_progress_estimate_when_provided() {
        let mut shape = shape_one();
        shape[0].last_acked_lsn = 400;
        let lag = LagConfig {
            records_per_ms: Some(10.0), // 10 records/ms → 100 records = 10 ms
            ..lag_default()
        };
        let topo = TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 500, &lag);
        assert_eq!(topo.replicas[0].lag_ms, 10);
    }

    #[test]
    fn topology_advertiser_lag_zero_rate_falls_back_to_u32_max() {
        // A degenerate rate (0 or negative) cannot produce a finite
        // ms estimate; the advertiser must not divide by zero.
        let mut shape = shape_one();
        shape[0].last_acked_lsn = 50;
        let lag = LagConfig {
            records_per_ms: Some(0.0),
            ..lag_default()
        };
        let topo = TopologyAdvertiser::advertise(&shape, &admin(), 1, primary_ep(), 500, &lag);
        assert_eq!(topo.replicas[0].lag_ms, u32::MAX);
    }

    // -----------------------------------------------------------
    // Epoch monotonicity: the advertiser is a pure function of the
    // epoch the caller passes — registry-change accounting belongs
    // to PrimaryReplication. The spec's "register, unregister, ack
    // timeout flipping healthy" assertions reduce to "the caller
    // is expected to bump the epoch on those events; the advertiser
    // surfaces what it gets". We pin the contract here so a future
    // refactor doesn't accidentally swallow the epoch.
    // -----------------------------------------------------------

    #[test]
    fn topology_advertiser_propagates_epoch_verbatim() {
        for epoch in [0, 1, 42, u64::MAX] {
            let topo = TopologyAdvertiser::advertise(
                &shape_one(),
                &admin(),
                epoch,
                primary_ep(),
                100,
                &lag_default(),
            );
            assert_eq!(topo.epoch, epoch);
        }
    }

    // -----------------------------------------------------------
    // Both transports invoke the advertiser; bytes match the
    // canonical encoder (#166). This is a server-internal sanity
    // check — the byte-for-byte round-trip across transports is
    // pinned by `reddb-grpc-proto::topology_tests` and
    // `reddb-wire::topology::tests`.
    // -----------------------------------------------------------

    #[test]
    fn topology_advertiser_output_round_trips_through_canonical_encoder() {
        use reddb_wire::topology::{decode_topology, encode_topology};
        let topo = TopologyAdvertiser::advertise(
            &shape_multi_region(),
            &admin(),
            13,
            primary_ep(),
            500,
            &lag_default(),
        );
        let bytes = encode_topology(&topo);
        let decoded = decode_topology(&bytes).expect("decode").expect("v1");
        assert_eq!(decoded, topo);
    }

    #[test]
    fn topology_advertiser_output_round_trips_through_hello_ack_wrapper() {
        use reddb_wire::topology::{decode_topology_from_hello_ack, encode_topology_for_hello_ack};
        let topo = TopologyAdvertiser::advertise(
            &shape_multi_region(),
            &operator(),
            21,
            primary_ep(),
            500,
            &lag_default(),
        );
        let field = encode_topology_for_hello_ack(&topo);
        let decoded = decode_topology_from_hello_ack(&field)
            .expect("decode")
            .expect("v1");
        assert_eq!(decoded, topo);
    }
}
