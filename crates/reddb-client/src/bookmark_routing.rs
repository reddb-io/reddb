//! Bookmark-aware routing table (issue #831, PRD #819).
//!
//! Projects the merged [`crate::topology::ClusterMembership`] into a
//! *driver-consumable* routing table:
//!
//! * **Write endpoint** — the current primary, keyed by replication
//!   `term`. Writes always route here; the term lets a caller detect
//!   that it is talking to a primary from a stale election.
//! * **Read endpoints** — every advertised replica carries its
//!   applied frontier (`last_applied_lsn`) and a *bookmark-eligibility*
//!   flag computed against a target bookmark. A replica is eligible
//!   when it is healthy and its contiguous applied frontier already
//!   covers the bookmark's commit LSN.
//!
//! ## Bounded-wait-then-fallback for causal reads
//!
//! [`RoutingTable::route_causal_read`] is the deep entry point. Given a
//! causal bookmark it:
//!
//! 1. Picks a *target* read replica (region-preferred, else the first
//!    healthy replica in advertised order).
//! 2. If that target's snapshot frontier already covers the bookmark,
//!    routes there immediately ([`RouteKind::EligibleTarget`]).
//! 3. Otherwise it waits, polling the target's live frontier through an
//!    injected [`BookmarkWaiter`], until either the target catches up
//!    ([`RouteKind::CaughtUpTarget`]) or the bounded deadline elapses.
//! 4. On deadline, it transparently falls back to *another* node whose
//!    snapshot frontier is already past the bookmark
//!    ([`RouteKind::FallbackReplica`]); if none qualifies, it falls back
//!    to the primary ([`RouteKind::FallbackPrimary`]), which is by
//!    definition past every committed bookmark.
//!
//! The method **never returns an error**: a lagging replica degrades a
//! causal read into a fallback hop, never a hard failure (issue #831
//! acceptance criterion 3). The wait/probe transport (an RPC against
//! the replica) is abstracted behind [`BookmarkWaiter`] so the routing
//! logic stays pure and unit-testable without a clock or a network.

use std::time::Duration;

use reddb_wire::replication::CausalBookmark;
use reddb_wire::topology::{Endpoint, ReplicaInfo};

use crate::topology::ClusterMembership;

/// Backwards-compatible client name for the wire causal bookmark token.
pub type BookmarkTarget = CausalBookmark;

/// The write-path endpoint: the current primary, tagged with the
/// replication term it is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEndpoint {
    pub addr: String,
    pub region: String,
    pub term: u64,
}

impl WriteEndpoint {
    /// True when this write endpoint serves `term`. A caller holding a
    /// bookmark from a newer term can use this to detect that the
    /// routing table is stale and force a topology refresh.
    pub fn serves_term(&self, term: u64) -> bool {
        self.term == term
    }

    /// Project back to the wire `Endpoint` shape for dialling.
    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            addr: self.addr.clone(),
            region: self.region.clone(),
        }
    }
}

/// A read-path endpoint candidate carrying its applied frontier and
/// whether it is eligible to serve a given bookmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadEndpoint {
    pub addr: String,
    pub region: String,
    pub healthy: bool,
    /// Lag estimate carried straight from the advertisement.
    pub lag_ms: u32,
    /// Contiguous applied frontier (`ReplicaInfo::last_applied_lsn`).
    pub frontier_lsn: u64,
    /// `true` while this replica is re-bootstrapping (issue #837). Its
    /// advertised frontier describes data it is about to discard, so it
    /// is never eligible for a causal read regardless of how far ahead
    /// that frontier sits.
    pub rebootstrapping: bool,
    /// `true` when this replica can serve the bookmark *now*: healthy,
    /// **not** re-bootstrapping, and its frontier already covers the
    /// bookmark commit LSN.
    pub bookmark_eligible: bool,
}

impl ReadEndpoint {
    fn from_replica(info: &ReplicaInfo, bookmark: BookmarkTarget) -> Self {
        // A re-bootstrapping replica is excluded even when its frontier
        // covers the bookmark: that frontier describes data it is about
        // to throw away on the atomic swap (issue #837).
        let eligible =
            info.healthy && !info.rebootstrapping && info.last_applied_lsn >= bookmark.commit_lsn();
        Self {
            addr: info.addr.clone(),
            region: info.region.clone(),
            healthy: info.healthy,
            lag_ms: info.lag_ms,
            frontier_lsn: info.last_applied_lsn,
            rebootstrapping: info.rebootstrapping,
            bookmark_eligible: eligible,
        }
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint {
            addr: self.addr.clone(),
            region: self.region.clone(),
        }
    }
}

/// Why a causal read landed on the endpoint it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// The request required a strong read, so it routed to the
    /// primary/range-owner path.
    StrongPrimary,
    /// The chosen target replica's snapshot frontier already covered
    /// the bookmark — no wait was needed.
    EligibleTarget,
    /// The target replica was behind but caught up within the deadline
    /// while we waited.
    CaughtUpTarget,
    /// The target stayed behind past the deadline; routed to another
    /// replica already past the bookmark.
    FallbackReplica,
    /// No replica was past the bookmark; routed to the primary, which
    /// is always past every committed bookmark.
    FallbackPrimary,
    /// Bounded-staleness routing found a healthy replica within the
    /// caller's declared lag bound.
    BoundedStalenessReplica,
    /// Local routing selected a healthy replica without making any
    /// freshness claim.
    LocalReplica,
}

impl RouteKind {
    /// True when the read was served by a fallback hop rather than the
    /// originally-chosen target replica.
    pub fn is_fallback(self) -> bool {
        matches!(self, Self::FallbackReplica | Self::FallbackPrimary)
    }
}

/// The resolved route for a causal read. Always present — the routing
/// table never turns a lagging replica into a hard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    /// The endpoint to dial.
    pub endpoint: Endpoint,
    /// How the decision was reached.
    pub kind: RouteKind,
    /// How long the bounded wait took before the decision settled.
    pub waited: Duration,
}

/// Drives the bounded wait against a target replica.
///
/// Each [`Self::poll`] blocks for one poll interval (clamped by the
/// remaining deadline), then reports the target replica's *current*
/// contiguous applied frontier — in production by issuing a lightweight
/// frontier RPC. [`Self::elapsed`] reports the time spent so far so the
/// routing table can enforce the deadline without owning a clock.
///
/// Keeping the transport behind this trait lets the wait-then-fallback
/// logic be exercised deterministically with a scripted fake.
pub trait BookmarkWaiter {
    /// Time elapsed since the wait began.
    fn elapsed(&self) -> Duration;
    /// Block for one poll interval, then return the target's current
    /// applied frontier LSN.
    fn poll(&mut self, target_addr: &str) -> u64;
}

/// Options for a causal read route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CausalReadOptions {
    /// Region the caller prefers to read from (locality). When set, a
    /// healthy replica in this region is chosen as the wait target
    /// ahead of out-of-region replicas.
    pub preferred_region: Option<String>,
    /// Upper bound on how long to wait for the target to catch up
    /// before falling back.
    pub deadline: Duration,
}

/// Per-request read consistency level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadConsistency {
    /// Route to the primary/range owner so the read never observes
    /// state before the commit watermark.
    Strong,
    /// Route using the session bookmark path.
    Causal {
        bookmark: BookmarkTarget,
        opts: CausalReadOptions,
    },
    /// Allow any healthy replica whose advertised lag is within
    /// `max_lag`.
    BoundedStaleness { max_lag: Duration },
    /// Allow any healthy member; does not block or claim freshness.
    Local,
}

/// Per-request query routing options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOptions {
    pub consistency: ReadConsistency,
}

impl QueryOptions {
    pub fn strong() -> Self {
        Self {
            consistency: ReadConsistency::Strong,
        }
    }

    pub fn causal(bookmark: BookmarkTarget, opts: CausalReadOptions) -> Self {
        Self {
            consistency: ReadConsistency::Causal { bookmark, opts },
        }
    }

    pub fn bounded_staleness(max_lag: Duration) -> Self {
        Self {
            consistency: ReadConsistency::BoundedStaleness { max_lag },
        }
    }

    pub fn local() -> Self {
        Self {
            consistency: ReadConsistency::Local,
        }
    }
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self::local()
    }
}

impl CausalReadOptions {
    /// Bounded wait with the given deadline and no region preference.
    pub fn with_deadline(deadline: Duration) -> Self {
        Self {
            preferred_region: None,
            deadline,
        }
    }

    /// Prefer reads from `region`.
    pub fn prefer_region(mut self, region: impl Into<String>) -> Self {
        self.preferred_region = Some(region.into());
        self
    }
}

/// Driver-consumable routing table derived from a topology
/// advertisement plus the current replication term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingTable {
    write: WriteEndpoint,
    replicas: Vec<ReplicaInfo>,
    epoch: u64,
}

impl RoutingTable {
    /// Build a routing table from a merged membership snapshot and the
    /// current replication `term`. The primary becomes the write
    /// endpoint; the advertised replicas become the read pool.
    pub fn from_membership(membership: ClusterMembership, term: u64) -> Self {
        let ClusterMembership {
            primary,
            replicas,
            epoch,
        } = membership;
        Self {
            write: WriteEndpoint {
                addr: primary.addr,
                region: primary.region,
                term,
            },
            replicas,
            epoch,
        }
    }

    /// Epoch of the advertisement this table was built from.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The write endpoint — the current primary, keyed by term.
    pub fn write_endpoint(&self) -> &WriteEndpoint {
        &self.write
    }

    /// The read endpoints with per-node frontier and bookmark
    /// eligibility computed against `bookmark`. Order matches the
    /// advertisement.
    pub fn read_endpoints(&self, bookmark: BookmarkTarget) -> Vec<ReadEndpoint> {
        self.replicas
            .iter()
            .map(|r| ReadEndpoint::from_replica(r, bookmark))
            .collect()
    }

    /// Pick the index of the wait target: a healthy, non-rebuilding
    /// replica, preferring `preferred_region`, otherwise the first such
    /// replica in advertised order. `None` when no replica can serve a
    /// causal read.
    ///
    /// A re-bootstrapping replica is skipped entirely (issue #837):
    /// there is no point waiting on or polling a node whose frontier
    /// describes data it is about to discard — it can never become a
    /// valid causal target until its swap completes and it re-advertises
    /// without the flag.
    fn pick_target_index(&self, preferred_region: Option<&str>) -> Option<usize> {
        if let Some(region) = preferred_region {
            if let Some(i) = self
                .replicas
                .iter()
                .position(|r| r.healthy && !r.rebootstrapping && r.region == region)
            {
                return Some(i);
            }
        }
        self.replicas
            .iter()
            .position(|r| r.healthy && !r.rebootstrapping)
    }

    /// Resolve a causal read with bounded-wait-then-fallback.
    ///
    /// Never errors: a lagging target degrades to a fallback hop
    /// (another caught-up replica, else the primary).
    pub fn route_causal_read(
        &self,
        bookmark: BookmarkTarget,
        opts: &CausalReadOptions,
        waiter: &mut dyn BookmarkWaiter,
    ) -> RouteDecision {
        let target_idx = self.pick_target_index(opts.preferred_region.as_deref());

        if let Some(idx) = target_idx {
            let target = &self.replicas[idx];

            // Fast path: the snapshot already shows the target past the
            // bookmark — route immediately, no wait.
            if target.last_applied_lsn >= bookmark.commit_lsn() {
                return RouteDecision {
                    endpoint: Endpoint {
                        addr: target.addr.clone(),
                        region: target.region.clone(),
                    },
                    kind: RouteKind::EligibleTarget,
                    waited: Duration::ZERO,
                };
            }

            // Bounded wait: poll the target's live frontier until it
            // catches up or the deadline elapses.
            let addr = target.addr.clone();
            let region = target.region.clone();
            while waiter.elapsed() < opts.deadline {
                let frontier = waiter.poll(&addr);
                if frontier >= bookmark.commit_lsn() {
                    return RouteDecision {
                        endpoint: Endpoint { addr, region },
                        kind: RouteKind::CaughtUpTarget,
                        waited: waiter.elapsed(),
                    };
                }
            }

            // Deadline blown — fall back below, excluding the target.
            return self.fall_back(bookmark, Some(idx), waiter.elapsed());
        }

        // No healthy replica at all — fall back straight away.
        self.fall_back(bookmark, None, waiter.elapsed())
    }

    /// Resolve a read for the requested per-request consistency
    /// level. Causal delegates to the bookmark route. Strong and
    /// fallback routes use the primary, which is the range-owner path
    /// at the current client routing layer.
    pub fn route_read(
        &self,
        options: &QueryOptions,
        waiter: &mut dyn BookmarkWaiter,
    ) -> RouteDecision {
        match &options.consistency {
            ReadConsistency::Strong => RouteDecision {
                endpoint: self.write.endpoint(),
                kind: RouteKind::StrongPrimary,
                waited: Duration::ZERO,
            },
            ReadConsistency::Causal { bookmark, opts } => {
                self.route_causal_read(*bookmark, opts, waiter)
            }
            ReadConsistency::BoundedStaleness { max_lag } => self.route_bounded_staleness(*max_lag),
            ReadConsistency::Local => self.route_local(),
        }
    }

    fn route_bounded_staleness(&self, max_lag: Duration) -> RouteDecision {
        let max_lag_ms = max_lag.as_millis().min(u32::MAX as u128) as u32;
        match self
            .replicas
            .iter()
            .find(|r| r.healthy && !r.rebootstrapping && r.lag_ms <= max_lag_ms)
        {
            Some(r) => RouteDecision {
                endpoint: Endpoint {
                    addr: r.addr.clone(),
                    region: r.region.clone(),
                },
                kind: RouteKind::BoundedStalenessReplica,
                waited: Duration::ZERO,
            },
            None => RouteDecision {
                endpoint: self.write.endpoint(),
                kind: RouteKind::FallbackPrimary,
                waited: Duration::ZERO,
            },
        }
    }

    fn route_local(&self) -> RouteDecision {
        match self.replicas.iter().find(|r| r.healthy) {
            Some(r) => RouteDecision {
                endpoint: Endpoint {
                    addr: r.addr.clone(),
                    region: r.region.clone(),
                },
                kind: RouteKind::LocalReplica,
                waited: Duration::ZERO,
            },
            None => RouteDecision {
                endpoint: self.write.endpoint(),
                kind: RouteKind::FallbackPrimary,
                waited: Duration::ZERO,
            },
        }
    }

    /// Fallback selection: a healthy replica (other than `exclude`)
    /// already past the bookmark, else the primary.
    fn fall_back(
        &self,
        bookmark: BookmarkTarget,
        exclude: Option<usize>,
        waited: Duration,
    ) -> RouteDecision {
        let caught_up = self.replicas.iter().enumerate().find(|(i, r)| {
            Some(*i) != exclude
                && r.healthy
                && !r.rebootstrapping
                && r.last_applied_lsn >= bookmark.commit_lsn()
        });
        match caught_up {
            Some((_, r)) => RouteDecision {
                endpoint: ReadEndpoint::from_replica(r, bookmark).endpoint(),
                kind: RouteKind::FallbackReplica,
                waited,
            },
            None => RouteDecision {
                endpoint: self.write.endpoint(),
                kind: RouteKind::FallbackPrimary,
                waited,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::topology::Endpoint as WireEndpoint;

    fn primary() -> WireEndpoint {
        WireEndpoint {
            addr: "primary:5050".into(),
            region: "us-east-1".into(),
        }
    }

    fn replica(addr: &str, region: &str, healthy: bool, frontier: u64) -> ReplicaInfo {
        ReplicaInfo {
            addr: addr.into(),
            region: region.into(),
            healthy,
            lag_ms: if healthy { 5 } else { u32::MAX },
            last_applied_lsn: frontier,
            rebootstrapping: false,
        }
    }

    fn lagging_replica(
        addr: &str,
        region: &str,
        healthy: bool,
        frontier: u64,
        lag_ms: u32,
    ) -> ReplicaInfo {
        ReplicaInfo {
            addr: addr.into(),
            region: region.into(),
            healthy,
            lag_ms,
            last_applied_lsn: frontier,
            rebootstrapping: false,
        }
    }

    /// A re-bootstrapping replica: healthy and far ahead on its
    /// advertised frontier, but rebuilding — so never causally
    /// eligible (issue #837).
    fn rebuilding_replica(addr: &str, region: &str, frontier: u64) -> ReplicaInfo {
        ReplicaInfo {
            addr: addr.into(),
            region: region.into(),
            healthy: true,
            lag_ms: 5,
            last_applied_lsn: frontier,
            rebootstrapping: true,
        }
    }

    fn membership(replicas: Vec<ReplicaInfo>) -> ClusterMembership {
        ClusterMembership {
            primary: primary(),
            replicas,
            epoch: 3,
        }
    }

    /// Scripted waiter: returns the next frontier in `steps` on each
    /// poll, advancing the elapsed clock by `tick` per poll. Once the
    /// script is exhausted it keeps returning the last value so the
    /// deadline (not the script) terminates the loop.
    struct ScriptedWaiter {
        steps: Vec<u64>,
        idx: usize,
        tick: Duration,
        elapsed: Duration,
        polled_addrs: Vec<String>,
    }

    impl ScriptedWaiter {
        fn new(steps: Vec<u64>, tick: Duration) -> Self {
            Self {
                steps,
                idx: 0,
                tick,
                elapsed: Duration::ZERO,
                polled_addrs: Vec::new(),
            }
        }
    }

    impl BookmarkWaiter for ScriptedWaiter {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }
        fn poll(&mut self, target_addr: &str) -> u64 {
            self.polled_addrs.push(target_addr.to_string());
            self.elapsed += self.tick;
            let v = self
                .steps
                .get(self.idx)
                .copied()
                .or_else(|| self.steps.last().copied())
                .unwrap_or(0);
            self.idx += 1;
            v
        }
    }

    // ---- write endpoint: primary by term ----

    #[test]
    fn write_endpoint_is_primary_keyed_by_term() {
        let table = RoutingTable::from_membership(membership(vec![]), 9);
        let w = table.write_endpoint();
        assert_eq!(w.addr, "primary:5050");
        assert_eq!(w.region, "us-east-1");
        assert_eq!(w.term, 9);
        assert!(w.serves_term(9));
        assert!(!w.serves_term(10));
    }

    // ---- read endpoints: per-node frontier + eligibility ----

    #[test]
    fn read_endpoints_carry_frontier_and_eligibility() {
        let table = RoutingTable::from_membership(
            membership(vec![
                replica("r-ahead:5050", "us-east-1", true, 200),
                replica("r-behind:5050", "us-east-1", true, 90),
                replica("r-down:5050", "us-west-2", false, 500),
            ]),
            1,
        );
        let bookmark = BookmarkTarget::new(1, 100);
        let reads = table.read_endpoints(bookmark);
        assert_eq!(reads.len(), 3);

        // Past the bookmark and healthy → eligible.
        assert_eq!(reads[0].frontier_lsn, 200);
        assert!(reads[0].bookmark_eligible);

        // Healthy but behind the commit LSN → ineligible.
        assert_eq!(reads[1].frontier_lsn, 90);
        assert!(!reads[1].bookmark_eligible);

        // Past the bookmark but unhealthy → ineligible.
        assert!(reads[2].frontier_lsn >= 100);
        assert!(!reads[2].healthy);
        assert!(!reads[2].bookmark_eligible);
    }

    #[test]
    fn eligibility_boundary_is_inclusive_at_commit_lsn() {
        let table =
            RoutingTable::from_membership(membership(vec![replica("r:5050", "r1", true, 100)]), 1);
        // frontier == commit_lsn must count as eligible.
        let reads = table.read_endpoints(BookmarkTarget::new(1, 100));
        assert!(reads[0].bookmark_eligible);
    }

    // ---- rebootstrapping replicas are excluded from causal reads ----

    #[test]
    fn rebootstrapping_replica_is_never_bookmark_eligible_despite_frontier() {
        // Frontier 999 is well past the bookmark, but the node is
        // rebuilding — its frontier describes data it will discard.
        let table = RoutingTable::from_membership(
            membership(vec![rebuilding_replica("rebuild:5050", "us-east-1", 999)]),
            1,
        );
        let reads = table.read_endpoints(BookmarkTarget::new(1, 100));
        assert_eq!(reads[0].frontier_lsn, 999);
        assert!(reads[0].rebootstrapping);
        assert!(
            !reads[0].bookmark_eligible,
            "a rebuilding node must never be bookmark-eligible"
        );
    }

    #[test]
    fn route_skips_rebuilding_node_and_falls_back_to_caught_up_peer() {
        // The rebuilding node is first in advertised order and far
        // ahead, but causal reads must bounce to the caught-up peer —
        // without ever polling the rebuilding node.
        let table = RoutingTable::from_membership(
            membership(vec![
                rebuilding_replica("rebuild:5050", "us-east-1", 999),
                replica("caught-up:5050", "us-east-1", true, 300),
            ]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        // The caught-up peer is the target chosen straight away; the
        // rebuilding node was never selected as the wait target.
        assert_eq!(decision.endpoint.addr, "caught-up:5050");
        assert!(!decision.kind.is_fallback());
        assert!(
            waiter.polled_addrs.iter().all(|a| a != "rebuild:5050"),
            "must never poll a rebuilding node"
        );
    }

    #[test]
    fn route_falls_back_to_primary_when_every_replica_is_rebuilding() {
        // All replicas are rebuilding (and ahead of the bookmark);
        // a causal read must still resolve — to the primary.
        let table = RoutingTable::from_membership(
            membership(vec![
                rebuilding_replica("rebuild-a:5050", "us-east-1", 999),
                rebuilding_replica("rebuild-b:5050", "us-west-2", 999),
            ]),
            4,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(4, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::FallbackPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
        assert!(
            waiter.polled_addrs.is_empty(),
            "no rebuilding node should be polled"
        );
    }

    #[test]
    fn rebuilding_node_excluded_as_fallback_target() {
        // The wait target (region-preferred, lagging) never catches up;
        // the only frontier-ahead peer is rebuilding, so the fallback
        // skips it and lands on the primary rather than serving a
        // bookmark from data about to be discarded.
        let table = RoutingTable::from_membership(
            membership(vec![
                replica("east-lag:5050", "us-east-1", true, 10),
                rebuilding_replica("west-rebuild:5050", "us-west-2", 999),
            ]),
            2,
        );
        let mut waiter = ScriptedWaiter::new(vec![10, 20, 30], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(2, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(25)).prefer_region("us-east-1"),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::FallbackPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
    }

    // ---- route: immediate eligible target (no wait) ----

    #[test]
    fn route_picks_eligible_target_without_waiting() {
        let table = RoutingTable::from_membership(
            membership(vec![replica("r-ok:5050", "us-east-1", true, 150)]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::EligibleTarget);
        assert_eq!(decision.endpoint.addr, "r-ok:5050");
        assert_eq!(decision.waited, Duration::ZERO);
        assert!(waiter.polled_addrs.is_empty(), "must not poll on fast path");
    }

    #[test]
    fn route_prefers_region_for_the_target() {
        let table = RoutingTable::from_membership(
            membership(vec![
                replica("east:5050", "us-east-1", true, 150),
                replica("west:5050", "us-west-2", true, 150),
            ]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500))
                .prefer_region("us-west-2"),
            &mut waiter,
        );
        assert_eq!(decision.endpoint.addr, "west:5050");
    }

    // ---- route: target catches up within the deadline ----

    #[test]
    fn route_waits_and_routes_to_target_once_it_catches_up() {
        let table = RoutingTable::from_membership(
            membership(vec![replica("r-lag:5050", "us-east-1", true, 50)]),
            1,
        );
        // Target is behind in the snapshot (50 < 100); it advances to
        // 100 on the third poll.
        let mut waiter = ScriptedWaiter::new(vec![60, 80, 100], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::CaughtUpTarget);
        assert_eq!(decision.endpoint.addr, "r-lag:5050");
        assert_eq!(decision.waited, Duration::from_millis(30));
        assert_eq!(waiter.polled_addrs.len(), 3);
    }

    // ---- route: target stays behind → fall back to a caught-up node ----

    #[test]
    fn route_falls_back_to_caught_up_replica_when_target_stays_behind() {
        let table = RoutingTable::from_membership(
            membership(vec![
                // Region-preferred target that never catches up.
                replica("east-lag:5050", "us-east-1", true, 10),
                // Out-of-region replica already past the bookmark.
                replica("west-ok:5050", "us-west-2", true, 300),
            ]),
            1,
        );
        // Deadline 25ms, tick 10ms → 3 polls, target frontier never
        // reaches 100.
        let mut waiter = ScriptedWaiter::new(vec![10, 20, 30], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(25)).prefer_region("us-east-1"),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::FallbackReplica);
        assert_eq!(decision.endpoint.addr, "west-ok:5050");
        assert!(decision.waited >= Duration::from_millis(25));
        // The fallback target was never the polled (lagging) node.
        assert!(waiter.polled_addrs.iter().all(|a| a == "east-lag:5050"));
    }

    // ---- route: no caught-up replica → fall back to primary ----

    #[test]
    fn route_falls_back_to_primary_when_no_replica_is_caught_up() {
        let table = RoutingTable::from_membership(
            membership(vec![
                replica("r1:5050", "us-east-1", true, 10),
                replica("r2:5050", "us-east-1", true, 20),
            ]),
            7,
        );
        let mut waiter = ScriptedWaiter::new(vec![10, 20], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(7, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(15)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::FallbackPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
        assert!(decision.kind.is_fallback());
    }

    #[test]
    fn route_falls_back_to_primary_when_no_replica_is_healthy() {
        let table = RoutingTable::from_membership(
            membership(vec![replica("r-down:5050", "us-east-1", false, 500)]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        // No healthy replica → straight to primary, no polling.
        assert_eq!(decision.kind, RouteKind::FallbackPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
        assert!(waiter.polled_addrs.is_empty());
    }

    #[test]
    fn route_falls_back_to_primary_when_no_replicas_advertised() {
        let table = RoutingTable::from_membership(membership(vec![]), 1);
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_causal_read(
            BookmarkTarget::new(1, 100),
            &CausalReadOptions::with_deadline(Duration::from_millis(500)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::FallbackPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
    }

    // ---- a lagging replica never produces a hard error ----

    #[test]
    fn lagging_replica_never_errors_always_resolves_an_endpoint() {
        // Whatever the topology shape, route_causal_read returns a
        // dialable endpoint — never a panic, never an Err.
        let shapes = vec![
            vec![],
            vec![replica("a:5050", "r1", true, 0)],
            vec![replica("a:5050", "r1", false, 0)],
            vec![
                replica("a:5050", "r1", true, 1),
                replica("b:5050", "r2", true, 999),
            ],
        ];
        for shape in shapes {
            let table = RoutingTable::from_membership(membership(shape), 1);
            let mut waiter = ScriptedWaiter::new(vec![0], Duration::from_millis(10));
            let decision = table.route_causal_read(
                BookmarkTarget::new(1, 100),
                &CausalReadOptions::with_deadline(Duration::from_millis(20)),
                &mut waiter,
            );
            assert!(!decision.endpoint.addr.is_empty());
        }
    }

    #[test]
    fn route_read_strong_uses_primary_without_polling() {
        let table = RoutingTable::from_membership(
            membership(vec![replica("r-ok:5050", "us-east-1", true, 150)]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![150], Duration::from_millis(10));
        let decision = table.route_read(&QueryOptions::strong(), &mut waiter);
        assert_eq!(decision.kind, RouteKind::StrongPrimary);
        assert_eq!(decision.endpoint.addr, "primary:5050");
        assert!(waiter.polled_addrs.is_empty());
    }

    #[test]
    fn route_read_bounded_staleness_respects_declared_lag_bound() {
        let table = RoutingTable::from_membership(
            membership(vec![
                lagging_replica("too-stale:5050", "us-east-1", true, 500, 250),
                lagging_replica("fresh-enough:5050", "us-east-1", true, 90, 40),
            ]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![], Duration::from_millis(10));
        let decision = table.route_read(
            &QueryOptions::bounded_staleness(Duration::from_millis(50)),
            &mut waiter,
        );
        assert_eq!(decision.kind, RouteKind::BoundedStalenessReplica);
        assert_eq!(decision.endpoint.addr, "fresh-enough:5050");
        assert!(waiter.polled_addrs.is_empty());

        let fallback = table.route_read(
            &QueryOptions::bounded_staleness(Duration::from_millis(10)),
            &mut waiter,
        );
        assert_eq!(fallback.kind, RouteKind::FallbackPrimary);
        assert_eq!(fallback.endpoint.addr, "primary:5050");
    }

    #[test]
    fn route_read_local_never_blocks_on_freshness() {
        let table = RoutingTable::from_membership(
            membership(vec![lagging_replica(
                "very-stale:5050",
                "us-east-1",
                true,
                0,
                u32::MAX,
            )]),
            1,
        );
        let mut waiter = ScriptedWaiter::new(vec![0, 0, 0], Duration::from_millis(10));
        let decision = table.route_read(&QueryOptions::local(), &mut waiter);
        assert_eq!(decision.kind, RouteKind::LocalReplica);
        assert_eq!(decision.endpoint.addr, "very-stale:5050");
        assert_eq!(decision.waited, Duration::ZERO);
        assert!(waiter.polled_addrs.is_empty());
    }
}
