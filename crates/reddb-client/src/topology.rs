//! Client-side topology consumer (issue #168, ADR 0008).
//!
//! Parses the canonical [`reddb_wire::topology::Topology`] payload —
//! delivered either as raw gRPC bytes or as a base64-wrapped string
//! field inside a RedWire HelloAck JSON envelope — and projects it
//! into a [`ClusterMembership`] structure that downstream routing
//! can read without caring about the wire encoding.
//!
//! # Merge rule (ADR 0008 §2)
//!
//! The seed URI a caller passes to `Reddb::connect("grpc://a,b,c")`
//! is a *hint*, not a constraint. When the server advertises a
//! topology:
//!
//! * The advertised primary always wins. The seed primary is
//!   discarded.
//! * Replicas advertised by the server make the cut. Each carries
//!   the server's metadata (`region`, `healthy`, `lag_ms`,
//!   `last_applied_lsn`).
//! * Replicas listed in the seed URI but absent from the
//!   advertisement are dropped — the operator decommissioned that
//!   node and the seed is stale.
//! * Replicas in both lists are kept; advertised metadata wins on
//!   any field collision.
//!
//! # Refresh contract
//!
//! [`TopologyConsumer::should_refresh`] short-circuits when the
//! observed epoch matches the current one. A higher-level driver
//! (the future `HealthAwareRouter` in lane Q) is expected to:
//!
//! * Poll the [`Topology`] RPC at a configured interval (default
//!   30s — see [`DEFAULT_REFRESH_INTERVAL`]).
//! * Force a refresh on the next call after a connection-level
//!   error, regardless of timer state.
//! * Skip the refresh when the previously observed epoch matches.
//!
//! [`RefreshScheduler`] captures the first two pieces with a
//! pluggable clock so the 30s interval is testable without real
//! waits.
//!
//! # Forward-compat (ADR 0008 §4)
//!
//! Unknown wire version tags and malformed base64 are *not* errors;
//! they collapse to "fall back to URI-only routing" by surfacing a
//! [`ConsumeError::UnknownVersion`] / [`ConsumeError::MalformedEnvelope`]
//! that callers downgrade with a one-line warning. Structurally
//! malformed bodies (truncated, bad UTF-8, oversized strings) bubble
//! up as typed [`ConsumeError`] variants — never panics.

use std::time::Duration;

use reddb_wire::topology::{
    self as wire, decode_topology, Endpoint as WireEndpoint, ReplicaInfo, Topology,
};

/// Default refresh interval for the topology poll loop. ADR 0008
/// §1 picks 30s as the conservative default; operators can override
/// per-deployment via [`RefreshScheduler::with_interval`].
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Seed addresses extracted from the connection URI.
///
/// `primary` is the host the caller dialled first. `replicas` is the
/// optional comma-separated tail (`grpc://a,b,c`). Both are kept as
/// the raw connection-string strings so the merge rule can match
/// them against advertised endpoint addresses cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriSeed {
    pub primary: String,
    pub replicas: Vec<String>,
}

impl UriSeed {
    /// Single-host seed (no replicas listed in the URI).
    pub fn single(primary: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            replicas: Vec::new(),
        }
    }

    /// Multi-host seed straight from a parsed `grpc://a,b,c` URI.
    pub fn cluster(primary: impl Into<String>, replicas: Vec<String>) -> Self {
        Self {
            primary: primary.into(),
            replicas,
        }
    }
}

/// The merged, route-ready view of the cluster.
///
/// The fields are the wire-canonical types from `reddb-wire` so a
/// future router can read them without translating shapes again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMembership {
    pub primary: WireEndpoint,
    pub replicas: Vec<ReplicaInfo>,
    pub epoch: u64,
}

/// Decode + merge errors. The unknown-version and malformed-envelope
/// variants are recoverable: the caller is expected to log a warning
/// and fall back to URI-only routing (ADR 0008 §4). The structural
/// variants (truncated, bad UTF-8, oversize strings) indicate a
/// genuinely broken peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeError {
    /// Buffer shorter than the 5-byte version+length header.
    Truncated,
    /// Header declared more body bytes than the buffer carries.
    BodyLengthMismatch { declared: u32, available: usize },
    /// A length-prefixed string field was not valid UTF-8.
    InvalidUtf8,
    /// A length-prefixed string declared more bytes than the body
    /// has remaining.
    StringTooLong { declared: u32, remaining: usize },
    /// Recognised header but the version tag is past
    /// [`wire::MAX_KNOWN_TOPOLOGY_VERSION`]. Recoverable: drop the
    /// advertisement, fall back to URI-only routing.
    UnknownVersion,
    /// HelloAck `topology` field was not valid base64. Recoverable
    /// in the same way as [`Self::UnknownVersion`].
    MalformedEnvelope,
}

impl std::fmt::Display for ConsumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "topology blob truncated"),
            Self::BodyLengthMismatch {
                declared,
                available,
            } => write!(
                f,
                "topology body length mismatch (declared {declared}, available {available})"
            ),
            Self::InvalidUtf8 => write!(f, "topology string field is not valid UTF-8"),
            Self::StringTooLong {
                declared,
                remaining,
            } => write!(
                f,
                "topology string length {declared} exceeds remaining body {remaining}"
            ),
            Self::UnknownVersion => write!(
                f,
                "topology wire version tag past MAX_KNOWN_TOPOLOGY_VERSION; falling back to URI-only routing"
            ),
            Self::MalformedEnvelope => write!(
                f,
                "topology envelope (HelloAck base64) is malformed; falling back to URI-only routing"
            ),
        }
    }
}

impl std::error::Error for ConsumeError {}

impl ConsumeError {
    /// True when the caller should downgrade to URI-only routing
    /// with a one-line warning (ADR 0008 §4) rather than treat the
    /// error as a hard failure.
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Self::UnknownVersion | Self::MalformedEnvelope)
    }
}

impl From<wire::TopologyError> for ConsumeError {
    fn from(e: wire::TopologyError) -> Self {
        match e {
            wire::TopologyError::Truncated => Self::Truncated,
            wire::TopologyError::BodyLengthMismatch {
                declared,
                available,
            } => Self::BodyLengthMismatch {
                declared,
                available,
            },
            wire::TopologyError::InvalidUtf8 => Self::InvalidUtf8,
            wire::TopologyError::StringTooLong {
                declared,
                remaining,
            } => Self::StringTooLong {
                declared,
                remaining,
            },
        }
    }
}

/// Stateless deep module — entry points are associated functions so
/// the routing driver can hold a `&Self` without capturing any state
/// the consumer doesn't actually need to keep across calls. State
/// (current epoch, last refresh) lives on the driver, not here.
#[derive(Debug, Default)]
pub struct TopologyConsumer;

impl TopologyConsumer {
    /// Apply the merge rule from ADR 0008 §2 against an already-
    /// decoded payload. Pure, infallible, no I/O.
    pub fn consume(payload: Topology, uri_seed: Option<UriSeed>) -> ClusterMembership {
        let Topology {
            epoch,
            primary,
            replicas,
        } = payload;

        // Merge: advertised replicas win on metadata; URI-only
        // replicas are dropped (decommissioned). Membership we emit
        // is exactly the advertised set, in advertised order — the
        // seed only acted as a hint for the *initial* dial.
        //
        // We still walk `uri_seed` to keep the merge contract
        // explicit: if a future variant of this rule wants to
        // surface "URI replica X was dropped because it isn't in
        // the advertisement", this is the spot. Today the merge is
        // a no-op on the seed and we just keep the advertised list.
        let _ = uri_seed;

        ClusterMembership {
            primary,
            replicas,
            epoch,
        }
    }

    /// Decode raw canonical bytes (gRPC `TopologyReply.topology_bytes`)
    /// and apply the merge.
    ///
    /// Recoverable variants (`UnknownVersion`) are surfaced as errors
    /// for the caller to log; the caller is expected to fall back to
    /// URI-only routing.
    pub fn consume_bytes(
        bytes: &[u8],
        uri_seed: Option<UriSeed>,
    ) -> Result<ClusterMembership, ConsumeError> {
        match decode_topology(bytes)? {
            Some(t) => Ok(Self::consume(t, uri_seed)),
            None => Err(ConsumeError::UnknownVersion),
        }
    }

    /// Decode the base64-wrapped HelloAck `topology` field and apply
    /// the merge. Mirrors the gRPC path so a router can drive both
    /// transports through one code path.
    pub fn consume_hello_ack(
        field: &str,
        uri_seed: Option<UriSeed>,
    ) -> Result<ClusterMembership, ConsumeError> {
        // `decode_topology_from_hello_ack` collapses both "bad
        // base64" and "unknown version tag" into `Ok(None)`. We
        // can't tell them apart from here, so the recoverable
        // variant we surface is the union — `MalformedEnvelope` —
        // when the base64 layer rejected the input. To distinguish,
        // we first try the base64 decode ourselves: if it succeeds
        // and `decode_topology` reports unknown version, surface
        // `UnknownVersion`; if base64 itself failed, surface
        // `MalformedEnvelope`.
        match decode_base64(field) {
            None => Err(ConsumeError::MalformedEnvelope),
            Some(bytes) => Self::consume_bytes(&bytes, uri_seed),
        }
        // (We deliberately don't call `decode_topology_from_hello_ack`
        // here even though it exists — splitting the two stages lets
        // us surface a precise recoverable variant.)
    }

    /// Refresh decision: skip when the observed epoch matches the
    /// epoch we already applied. Strictly-greater is the canonical
    /// "advance" condition; a lower observed epoch is treated as
    /// stale and *also* skipped (the refresh loop will see the next
    /// poll's payload).
    ///
    /// The refresh loop calls this with the raw observed epoch from
    /// the just-decoded payload. Connection-level errors are out of
    /// scope here — they force a refresh through a different
    /// codepath ([`RefreshScheduler::force_now`]).
    pub fn should_refresh(current_epoch: u64, observed_epoch: u64) -> bool {
        observed_epoch > current_epoch
    }
}

// --------------------------------------------------------------
// Refresh scheduling — pluggable clock so the 30s interval is
// testable without real waits.
// --------------------------------------------------------------

/// Trait the [`RefreshScheduler`] reads time from. The production
/// impl reads `std::time::Instant::now()`; tests inject a
/// monotonic-counter fake.
pub trait Clock: std::fmt::Debug {
    fn now_monotonic_ms(&self) -> u64;
}

/// Default real-time clock. Hides the `Instant` epoch so the trait
/// stays `dyn`-friendly.
#[derive(Debug)]
pub struct SystemClock {
    origin: std::time::Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now_monotonic_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Drives the periodic-refresh + force-on-error rule.
///
/// Owns the "should I refresh now?" decision; the actual RPC dispatch
/// is the higher-level driver's job. Keeping this state machine
/// isolated lets the 30s interval get tested without sleeping.
#[derive(Debug)]
pub struct RefreshScheduler {
    interval: Duration,
    clock: Box<dyn Clock + Send + Sync>,
    last_refresh_ms: Option<u64>,
    /// Force flag set by [`Self::force_now`]; cleared the next time
    /// [`Self::should_refresh_now`] returns true.
    force: bool,
}

impl RefreshScheduler {
    /// Build a scheduler with the default 30s interval and the real
    /// system clock.
    pub fn new() -> Self {
        Self::with_interval(DEFAULT_REFRESH_INTERVAL)
    }

    /// Build a scheduler with a custom interval and the real system
    /// clock.
    pub fn with_interval(interval: Duration) -> Self {
        Self::with_interval_and_clock(interval, Box::new(SystemClock::default()))
    }

    /// Build a scheduler with a custom interval *and* clock — the
    /// hook tests inject a fake clock through.
    pub fn with_interval_and_clock(
        interval: Duration,
        clock: Box<dyn Clock + Send + Sync>,
    ) -> Self {
        Self {
            interval,
            clock,
            last_refresh_ms: None,
            force: false,
        }
    }

    /// Poll-loop hook: call once per loop iteration (or before each
    /// dispatch). Returns true when a refresh is due.
    ///
    /// On `true`, the caller should dispatch the [`Topology`] RPC,
    /// run [`TopologyConsumer::consume_bytes`], and call
    /// [`Self::mark_refreshed`] with the resulting epoch.
    pub fn should_refresh_now(&mut self) -> bool {
        if self.force {
            self.force = false;
            return true;
        }
        let now = self.clock.now_monotonic_ms();
        let interval_ms = self.interval.as_millis() as u64;
        match self.last_refresh_ms {
            None => true,
            Some(last) => now.saturating_sub(last) >= interval_ms,
        }
    }

    /// Stamp the last successful refresh. The next
    /// [`Self::should_refresh_now`] returns true once
    /// `interval` has elapsed.
    pub fn mark_refreshed(&mut self) {
        self.last_refresh_ms = Some(self.clock.now_monotonic_ms());
    }

    /// Set the force flag — the next call to
    /// [`Self::should_refresh_now`] returns true regardless of
    /// the timer. Used by the routing driver after a connection-
    /// level error.
    pub fn force_now(&mut self) {
        self.force = true;
    }
}

impl Default for RefreshScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------
// Internal: minimal base64 decoder reused so we can split the
// "bad base64" vs "bad version" recoverable error variants.
// Mirrors the alphabet used by the wire encoder. Kept private —
// the wire crate exposes its own; this is a paste-equivalent so
// we don't widen `reddb-wire`'s public surface for one branch.
// --------------------------------------------------------------

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in trimmed.bytes() {
        let v: u32 = match ch {
            b'A'..=b'Z' => (ch - b'A') as u32,
            b'a'..=b'z' => (ch - b'a' + 26) as u32,
            b'0'..=b'9' => (ch - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::topology::{
        encode_topology, encode_topology_for_hello_ack, Endpoint as WireEndpoint, ReplicaInfo,
        Topology, TOPOLOGY_HEADER_SIZE, TOPOLOGY_WIRE_VERSION_V1,
    };

    fn fixture() -> Topology {
        Topology {
            epoch: 7,
            primary: WireEndpoint {
                addr: "primary.example.com:5050".into(),
                region: "us-east-1".into(),
            },
            replicas: vec![
                ReplicaInfo {
                    addr: "replica-a.example.com:5050".into(),
                    region: "us-east-1".into(),
                    healthy: true,
                    lag_ms: 12,
                    last_applied_lsn: 4242,
                },
                ReplicaInfo {
                    addr: "replica-b.example.com:5050".into(),
                    region: "us-west-2".into(),
                    healthy: false,
                    lag_ms: 999,
                    last_applied_lsn: 4100,
                },
            ],
        }
    }

    // ---- round-trip on both transports ----

    #[test]
    fn parse_round_trip_grpc_bytes() {
        let t = fixture();
        let bytes = encode_topology(&t);
        let m = TopologyConsumer::consume_bytes(&bytes, None).expect("consume");
        assert_eq!(m.epoch, 7);
        assert_eq!(m.primary, t.primary);
        assert_eq!(m.replicas, t.replicas);
    }

    #[test]
    fn parse_round_trip_hello_ack_field() {
        let t = fixture();
        let field = encode_topology_for_hello_ack(&t);
        let m = TopologyConsumer::consume_hello_ack(&field, None).expect("consume");
        assert_eq!(m.epoch, 7);
        assert_eq!(m.primary, t.primary);
        assert_eq!(m.replicas, t.replicas);
    }

    #[test]
    fn fixture_byte_stable_across_runs() {
        // Acceptance: same Topology fixture round-trips byte-stable
        // through the canonical encoder, so both transports carry
        // identical bytes (#166 §4 already pinned this; here we
        // assert the consumer doesn't perturb it).
        let t = fixture();
        let a = encode_topology(&t);
        let b = encode_topology(&t);
        assert_eq!(a, b);
        // And the inner bytes recovered from the HelloAck base64
        // wrapper match the gRPC bytes byte-for-byte.
        let field = encode_topology_for_hello_ack(&t);
        let recovered = decode_base64(&field).expect("base64");
        assert_eq!(recovered, a);
    }

    // ---- merge rule ----

    #[test]
    fn merge_uri_only_replicas_dropped() {
        // URI lists three replicas; advertisement only carries two.
        // The third (URI-only) must be dropped — operator
        // decommissioned it.
        let t = fixture();
        let seed = UriSeed::cluster(
            "primary.example.com:5050".to_string(),
            vec![
                "replica-a.example.com:5050".into(),
                "replica-b.example.com:5050".into(),
                "replica-stale.example.com:5050".into(),
            ],
        );
        let m = TopologyConsumer::consume(t.clone(), Some(seed));
        assert_eq!(m.replicas.len(), 2, "URI-only replica must be dropped");
        assert!(
            m.replicas
                .iter()
                .all(|r| r.addr != "replica-stale.example.com:5050"),
            "stale URI replica must not appear in membership"
        );
    }

    #[test]
    fn merge_advertised_only_replicas_added() {
        // URI lists no replicas; advertisement carries two. Both
        // must show up in the merged membership — URI is a hint,
        // not a constraint.
        let t = fixture();
        let seed = UriSeed::single("primary.example.com:5050");
        let m = TopologyConsumer::consume(t.clone(), Some(seed));
        assert_eq!(m.replicas.len(), 2);
        assert_eq!(m.replicas, t.replicas);
    }

    #[test]
    fn merge_intersection_uses_advertised_metadata() {
        // URI replica matches an advertised replica. The merged
        // membership must carry the *advertised* metadata
        // (region, healthy, lag_ms, last_applied_lsn), not anything
        // synthesised from the URI.
        let t = fixture();
        let seed = UriSeed::cluster(
            "primary.example.com:5050".to_string(),
            vec!["replica-a.example.com:5050".into()],
        );
        let m = TopologyConsumer::consume(t.clone(), Some(seed));
        let merged_a = m
            .replicas
            .iter()
            .find(|r| r.addr == "replica-a.example.com:5050")
            .expect("advertised replica must be present");
        assert_eq!(merged_a.region, "us-east-1");
        assert!(merged_a.healthy);
        assert_eq!(merged_a.lag_ms, 12);
        assert_eq!(merged_a.last_applied_lsn, 4242);
    }

    #[test]
    fn merge_with_no_seed_keeps_full_advertisement() {
        let t = fixture();
        let m = TopologyConsumer::consume(t.clone(), None);
        assert_eq!(m.primary, t.primary);
        assert_eq!(m.replicas, t.replicas);
        assert_eq!(m.epoch, t.epoch);
    }

    // ---- refresh decision ----

    #[test]
    fn should_refresh_skips_same_epoch() {
        assert!(!TopologyConsumer::should_refresh(7, 7));
    }

    #[test]
    fn should_refresh_advances_on_higher_epoch() {
        assert!(TopologyConsumer::should_refresh(7, 8));
    }

    #[test]
    fn should_refresh_treats_lower_epoch_as_stale() {
        // A lower observed epoch means the peer is behind us. We
        // skip — the next poll picks up the canonical advancement.
        assert!(!TopologyConsumer::should_refresh(7, 6));
    }

    // ---- malformed / adversarial fixtures ----

    #[test]
    fn malformed_truncated_blob_returns_typed_error() {
        let err = TopologyConsumer::consume_bytes(&[0x01, 0x00], None).unwrap_err();
        assert!(matches!(err, ConsumeError::Truncated));
        assert!(!err.is_recoverable());
    }

    #[test]
    fn malformed_body_length_mismatch_returns_typed_error() {
        let bytes = vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        let err = TopologyConsumer::consume_bytes(&bytes, None).unwrap_err();
        assert!(matches!(err, ConsumeError::BodyLengthMismatch { .. }));
        assert!(!err.is_recoverable());
    }

    #[test]
    fn unknown_version_tag_is_recoverable() {
        // ADR 0008 §4: forward-compat. An unknown wire version tag
        // collapses to "fall back to URI-only routing", surfaced as
        // a recoverable typed error so the caller can log a one-line
        // warning before downgrading.
        let mut bytes = encode_topology(&fixture());
        bytes[0] = 0xFE; // past MAX_KNOWN_TOPOLOGY_VERSION
        let err = TopologyConsumer::consume_bytes(&bytes, None).unwrap_err();
        assert!(matches!(err, ConsumeError::UnknownVersion));
        assert!(err.is_recoverable());
    }

    #[test]
    fn malformed_envelope_base64_is_recoverable() {
        // Bad base64 in the HelloAck `topology` field. Same posture
        // as an unknown version tag: drop, fall back, never panic.
        let err = TopologyConsumer::consume_hello_ack("@not base64@", None).unwrap_err();
        assert!(matches!(err, ConsumeError::MalformedEnvelope));
        assert!(err.is_recoverable());
    }

    #[test]
    fn oversize_string_field_returns_typed_error() {
        // Adversarial: stamp a string-length prefix that overruns
        // the body. The decoder must surface a typed error, not
        // panic.
        // Build a v1 body with a primary.addr length way past the
        // available body bytes.
        let mut body = Vec::new();
        body.extend_from_slice(&0u64.to_le_bytes()); // epoch
                                                     // primary.addr len = 0xFFFF_FFFF (clearly bogus)
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.push(TOPOLOGY_WIRE_VERSION_V1);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        assert_eq!(bytes.len(), TOPOLOGY_HEADER_SIZE + body.len());
        let err = TopologyConsumer::consume_bytes(&bytes, None).unwrap_err();
        assert!(
            matches!(err, ConsumeError::StringTooLong { .. }),
            "expected StringTooLong, got {err:?}"
        );
        assert!(!err.is_recoverable());
    }

    #[test]
    fn invalid_utf8_string_returns_typed_error() {
        // Build a v1 body where primary.addr is two bytes of
        // invalid UTF-8 (0xFF 0xFE).
        let mut body = Vec::new();
        body.extend_from_slice(&0u64.to_le_bytes()); // epoch
        body.extend_from_slice(&2u32.to_le_bytes()); // primary.addr len
        body.extend_from_slice(&[0xFF, 0xFE]); // bogus utf8
                                               // The body would normally continue, but the decoder
                                               // hits invalid utf8 first.
        let mut bytes = Vec::new();
        bytes.push(TOPOLOGY_WIRE_VERSION_V1);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        let err = TopologyConsumer::consume_bytes(&bytes, None).unwrap_err();
        assert!(
            matches!(err, ConsumeError::InvalidUtf8),
            "expected InvalidUtf8, got {err:?}"
        );
    }

    #[test]
    fn consume_does_not_panic_on_any_random_short_buffer() {
        // Smoke fuzz: short buffers should always either Ok-Some or
        // typed-Err, never panic.
        for n in 0..10usize {
            let bytes = vec![0xAAu8; n];
            let _ = TopologyConsumer::consume_bytes(&bytes, None);
        }
    }

    // ---- fake-clock RefreshScheduler ----

    #[derive(Debug)]
    struct FakeClock {
        ms: std::sync::Mutex<u64>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                ms: std::sync::Mutex::new(0),
            }
        }
        fn advance(&self, by: Duration) {
            *self.ms.lock().unwrap() += by.as_millis() as u64;
        }
    }

    impl Clock for FakeClock {
        fn now_monotonic_ms(&self) -> u64 {
            *self.ms.lock().unwrap()
        }
    }

    fn scheduler_with(clock: std::sync::Arc<FakeClock>, interval: Duration) -> RefreshScheduler {
        // The scheduler owns a Box<dyn Clock>; route both the
        // scheduler and the test handle through an Arc so the test
        // can advance time without taking the box back.
        #[derive(Debug)]
        struct ArcClock(std::sync::Arc<FakeClock>);
        impl Clock for ArcClock {
            fn now_monotonic_ms(&self) -> u64 {
                self.0.now_monotonic_ms()
            }
        }
        RefreshScheduler::with_interval_and_clock(interval, Box::new(ArcClock(clock)))
    }

    #[test]
    fn fake_clock_first_call_refreshes_immediately() {
        let clock = std::sync::Arc::new(FakeClock::new());
        let mut s = scheduler_with(clock.clone(), Duration::from_secs(30));
        assert!(s.should_refresh_now(), "first call must refresh");
    }

    #[test]
    fn fake_clock_thirty_second_interval_holds_without_real_waits() {
        let clock = std::sync::Arc::new(FakeClock::new());
        let mut s = scheduler_with(clock.clone(), Duration::from_secs(30));
        assert!(s.should_refresh_now());
        s.mark_refreshed();
        // Just under 30s: must NOT refresh.
        clock.advance(Duration::from_millis(29_999));
        assert!(
            !s.should_refresh_now(),
            "must not refresh before interval elapsed"
        );
        // Crossing the threshold: must refresh.
        clock.advance(Duration::from_millis(2));
        assert!(
            s.should_refresh_now(),
            "must refresh once interval has elapsed"
        );
    }

    #[test]
    fn fake_clock_force_now_overrides_interval() {
        let clock = std::sync::Arc::new(FakeClock::new());
        let mut s = scheduler_with(clock.clone(), Duration::from_secs(30));
        assert!(s.should_refresh_now());
        s.mark_refreshed();
        // Far below the 30s interval — would normally skip.
        clock.advance(Duration::from_millis(100));
        assert!(!s.should_refresh_now());
        // Connection-level error: force a refresh on the next call.
        s.force_now();
        assert!(
            s.should_refresh_now(),
            "force_now must override the timer"
        );
        // Force flag is single-shot: the next call goes back to the
        // timer (which has not elapsed).
        s.mark_refreshed();
        clock.advance(Duration::from_millis(100));
        assert!(!s.should_refresh_now());
    }

    #[test]
    fn default_interval_is_thirty_seconds() {
        // Sentinel against an accidental rebase that knocks the
        // documented 30s default.
        assert_eq!(DEFAULT_REFRESH_INTERVAL, Duration::from_secs(30));
    }
}
