//! Cascading replication for async read-replicas (issue #838, PRD #819).
//!
//! # Why cascade
//!
//! A single primary streaming WAL to every replica pays an O(replicas)
//! fan-out cost: each connected replica is one more stream the primary must
//! frame, retain WAL for, and track. Read scale-out — adding many async
//! read-replicas — therefore loads the *primary*, the one node whose spare
//! capacity matters most because it also serves the write path.
//!
//! Cascading replication bounds that fan-out: an async read-replica may
//! stream from an **intermediate** replica instead of from the primary. The
//! intermediate holds the sub-replica's slot and forwards the WAL stream it
//! is already receiving. The primary sees one stream (to the intermediate)
//! regardless of how many sub-replicas hang off it.
//!
//! # Why voting members never cascade
//!
//! ADR 0030 keeps the durability/election path simple and fast: a quorum is a
//! majority of *voting* members, and a synchronous write is acknowledged only
//! once a quorum has it durably. If a voting member streamed through an
//! intermediate, every commit-ack and every election-relevant frontier would
//! pay an extra hop of lag, and an intermediate failure would stall a member
//! the consensus path depends on. So the rule is categorical: **a voting
//! member always streams directly from the primary**. Cascade is a
//! read-scale-out optimisation for members that are *not* in the durability
//! path. A voting member that is handed a cascade source refuses it and falls
//! back to the primary (see [`plan_upstream`]).
//!
//! # Frontier propagation
//!
//! Correctness of the chain rests on one invariant: **the primary must not
//! prune WAL that any node downstream of the chain still needs.** The
//! intermediate enforces this by reporting to its own upstream a *retention
//! frontier* that is the minimum of (a) what it has itself applied and (b)
//! what every sub-replica streaming through it has confirmed
//! ([`CascadeRelay::upstream_confirmed_lsn`]). A slow leaf therefore holds the
//! whole chain's slot open at the primary, exactly as if it were connected
//! directly — this is the cascaded analogue of PostgreSQL's
//! `hot_standby_feedback`.
//!
//! The read-visibility frontier flows the same direction: a causal
//! ([`CausalBookmark`]) read can only be satisfied at a node that has applied
//! up to the bookmark's `commit_lsn`. Down the chain the applied frontier is
//! monotonically non-increasing (a sub-replica can never be ahead of the
//! intermediate that feeds it), so
//! [`CascadeRelay::downstream_visible_frontier`] reports the highest LSN a
//! given sub-replica can serve.
//!
//! # Module shape
//!
//! This module is pure policy + bookkeeping with no I/O: [`plan_upstream`]
//! decides where a node connects, and [`CascadeRelay`] tracks the slots and
//! frontiers an intermediate holds for its sub-replicas. The transport that
//! actually forwards bytes composes these primitives, so the rules are
//! unit-testable without a network — the same discipline the election core
//! (issue #834) follows.

use std::collections::BTreeMap;

use crate::replication::bookmark::CausalBookmark;
use crate::replication::election::{Member, MemberKind, VotingState};

// ---------------------------------------------------------------
// Streaming class — who may cascade
// ---------------------------------------------------------------

/// How a node chooses its WAL upstream.
///
/// This is orthogonal to the election [`MemberKind`]/[`VotingState`] model: a
/// node's *streaming class* answers "may this node accept a cascade source?",
/// where the membership model answers "does this node vote / can it become
/// primary?". A witness has no data stream at all, so it is irrelevant here;
/// the meaningful split is between members on the durability path (which must
/// stream directly) and pure read scale-out replicas (which may cascade).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplicaClass {
    /// Participates in the durability/election quorum. Streams **directly**
    /// from the primary and refuses any cascade source (ADR 0030). This is
    /// the safe default: a node only cascades when explicitly declared a
    /// read-replica.
    #[default]
    Voting,
    /// Async read-scale-out replica. Not in the durability path, so it **may**
    /// stream from an intermediate replica to bound the primary's fan-out.
    AsyncReadReplica,
}

impl ReplicaClass {
    /// Derive the streaming class from an election membership view.
    ///
    /// Any member that currently counts toward quorum
    /// ([`Member::is_voter`]) is on the durability path and must stream
    /// directly; a data member that is non-voting (e.g. a read-replica that
    /// never joins the voter set) may cascade. This lets a caller that
    /// already holds a [`Member`] derive the cascade policy without
    /// re-declaring intent.
    pub fn from_member(member: &Member) -> Self {
        // A witness carries no data stream; a voting member is on the
        // durability path. Either way it must not cascade. Only a
        // non-voting *data* member is a candidate for read scale-out.
        match (member.kind, member.is_voter()) {
            (MemberKind::Data, false) => ReplicaClass::AsyncReadReplica,
            _ => ReplicaClass::Voting,
        }
    }

    /// Whether a node of this class is permitted to stream from an
    /// intermediate replica rather than the primary.
    pub fn may_cascade(self) -> bool {
        matches!(self, ReplicaClass::AsyncReadReplica)
    }
}

/// An intermediate replica a sub-replica may cascade from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeUpstream {
    /// Stable node identity of the intermediate (matches its replica id).
    pub node_id: String,
    /// Address the sub-replica connects to in order to stream from the
    /// intermediate (e.g. `"http://replica-a:55055"`).
    pub addr: String,
}

impl CascadeUpstream {
    pub fn new(node_id: impl Into<String>, addr: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            addr: addr.into(),
        }
    }
}

/// Where a node should open its WAL stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamChoice {
    /// Connect directly to the primary.
    Primary,
    /// Cascade from the named intermediate replica.
    Intermediate(CascadeUpstream),
}

impl UpstreamChoice {
    /// `true` when this choice streams from an intermediate (a cascade).
    pub fn is_cascade(&self) -> bool {
        matches!(self, UpstreamChoice::Intermediate(_))
    }
}

/// Why a requested cascade source was refused and the node fell back to the
/// primary. Surfaced (not swallowed) so a misconfiguration is observable
/// rather than a silent performance cliff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeRefusal {
    /// The node is on the durability path; a voting member always streams
    /// directly from the primary (ADR 0030).
    VotingMemberDirectOnly,
    /// The requested intermediate is the node itself — a node cannot cascade
    /// from its own slot.
    SelfReference,
}

impl CascadeRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VotingMemberDirectOnly => "voting-member-direct-only",
            Self::SelfReference => "self-reference",
        }
    }
}

/// Decide where a node streams from, given its streaming class and an
/// optionally-requested intermediate source.
///
/// The decision is total and side-effect-free:
///
/// * No requested intermediate → [`UpstreamChoice::Primary`], no refusal.
/// * Requested, but the node is a voting member → refuse with
///   [`CascadeRefusal::VotingMemberDirectOnly`] and fall back to the primary.
/// * Requested, but the intermediate is this node itself → refuse with
///   [`CascadeRefusal::SelfReference`] and fall back to the primary.
/// * Requested, node is an async read-replica, source is another node →
///   [`UpstreamChoice::Intermediate`], no refusal.
///
/// Returning the refusal alongside the (safe) fallback choice lets the caller
/// honour the connection immediately while still logging *why* a configured
/// cascade did not take effect.
pub fn plan_upstream(
    self_node_id: &str,
    class: ReplicaClass,
    requested: Option<&CascadeUpstream>,
) -> (UpstreamChoice, Option<CascadeRefusal>) {
    let Some(upstream) = requested else {
        return (UpstreamChoice::Primary, None);
    };
    if !class.may_cascade() {
        return (
            UpstreamChoice::Primary,
            Some(CascadeRefusal::VotingMemberDirectOnly),
        );
    }
    if upstream.node_id == self_node_id {
        return (UpstreamChoice::Primary, Some(CascadeRefusal::SelfReference));
    }
    (UpstreamChoice::Intermediate(upstream.clone()), None)
}

// ---------------------------------------------------------------
// CascadeRelay — an intermediate that holds slots and forwards
// ---------------------------------------------------------------

/// A sub-replica slot held by an intermediate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownstreamSlot {
    /// Identity of the sub-replica streaming through this intermediate.
    pub id: String,
    /// Highest LSN the sub-replica has confirmed durably applied. Drives the
    /// retention frontier reported upstream — the intermediate must keep WAL
    /// above this point so it can still forward it.
    pub confirmed_lsn: u64,
    /// Highest LSN forwarded to the sub-replica so far. Always
    /// `>= confirmed_lsn`; the gap is in-flight, not yet acked.
    pub sent_lsn: u64,
}

/// Tracks the sub-replica slots an intermediate holds and the frontiers that
/// must propagate through the chain. Pure bookkeeping — the forwarding
/// transport calls into it to decide what to send and what to advertise
/// upstream.
///
/// All LSN updates are monotonic: a stale ack or a duplicate forward can never
/// rewind a frontier, which keeps retention safe under reordering and retries.
#[derive(Debug, Clone)]
pub struct CascadeRelay {
    node_id: String,
    /// What this intermediate has itself applied from its own upstream. It can
    /// never forward beyond this point — it cannot forward records it does not
    /// yet hold.
    self_applied_lsn: u64,
    downstream: BTreeMap<String, DownstreamSlot>,
}

impl CascadeRelay {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            self_applied_lsn: 0,
            downstream: BTreeMap::new(),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Record how far this intermediate has applied from its own upstream.
    /// Monotonic — a late report never rewinds the forward bound.
    pub fn record_self_applied(&mut self, lsn: u64) {
        self.self_applied_lsn = self.self_applied_lsn.max(lsn);
    }

    pub fn self_applied_lsn(&self) -> u64 {
        self.self_applied_lsn
    }

    /// Hold a sub-replica's slot, resuming it at `start_lsn`.
    ///
    /// Idempotent on reconnect (issue #812 semantics): if the slot already
    /// exists its progress is preserved — only a *forward* `start_lsn` can
    /// advance `confirmed_lsn`, never rewind it — so a reconnecting
    /// sub-replica is not pushed backwards. Returns the LSN the sub-replica
    /// should resume streaming from (its retained confirmed position).
    pub fn register_downstream(&mut self, id: impl Into<String>, start_lsn: u64) -> u64 {
        let id = id.into();
        let slot = self
            .downstream
            .entry(id.clone())
            .or_insert_with(|| DownstreamSlot {
                id,
                confirmed_lsn: start_lsn,
                sent_lsn: start_lsn,
            });
        // Only advance on (re)registration; never rewind a live slot.
        slot.confirmed_lsn = slot.confirmed_lsn.max(start_lsn);
        slot.sent_lsn = slot.sent_lsn.max(slot.confirmed_lsn);
        slot.confirmed_lsn
    }

    /// Release a sub-replica's slot. Returns `true` if it was held. After
    /// this, the released sub-replica no longer pins the chain's retention
    /// frontier.
    pub fn unregister_downstream(&mut self, id: &str) -> bool {
        self.downstream.remove(id).is_some()
    }

    /// Record a sub-replica's confirmation that it has durably applied up to
    /// `lsn`. Monotonic. No-op for an unknown id.
    pub fn record_downstream_ack(&mut self, id: &str, lsn: u64) {
        if let Some(slot) = self.downstream.get_mut(id) {
            slot.confirmed_lsn = slot.confirmed_lsn.max(lsn);
            slot.sent_lsn = slot.sent_lsn.max(slot.confirmed_lsn);
        }
    }

    /// Note that records up to `lsn` were forwarded to a sub-replica.
    /// Monotonic. No-op for an unknown id.
    pub fn note_forwarded(&mut self, id: &str, lsn: u64) {
        if let Some(slot) = self.downstream.get_mut(id) {
            slot.sent_lsn = slot.sent_lsn.max(lsn);
        }
    }

    pub fn downstream_ids(&self) -> Vec<String> {
        self.downstream.keys().cloned().collect()
    }

    pub fn downstream_slot(&self, id: &str) -> Option<&DownstreamSlot> {
        self.downstream.get(id)
    }

    pub fn downstream_count(&self) -> usize {
        self.downstream.len()
    }

    /// The retention frontier this intermediate reports to its own upstream
    /// (the primary, or a further intermediate).
    ///
    /// This is the crux of chain correctness: it is the minimum of what the
    /// intermediate has itself applied and what *every* sub-replica has
    /// confirmed. The upstream retains WAL at or above this point, so a slow
    /// leaf keeps the whole chain's slot open — the primary never prunes a
    /// record some downstream node still needs.
    ///
    /// With no sub-replicas the frontier is simply the intermediate's own
    /// applied position (it behaves like an ordinary direct replica).
    pub fn upstream_confirmed_lsn(&self) -> u64 {
        match self
            .downstream
            .values()
            .map(|slot| slot.confirmed_lsn)
            .min()
        {
            // Clamp the slowest leaf by what we actually hold: a leaf can
            // never need WAL beyond what the intermediate has applied.
            Some(min_downstream) => min_downstream.min(self.self_applied_lsn),
            None => self.self_applied_lsn,
        }
    }

    /// The retention frontier as a causal bookmark, stamped with `term`.
    /// Lets the chain advertise its safe-to-prune point in the same token
    /// vocabulary causal reads use (ADR 0031).
    pub fn upstream_confirmed_bookmark(&self, term: u64) -> CausalBookmark {
        CausalBookmark::new(term, self.upstream_confirmed_lsn())
    }

    /// The highest LSN a given sub-replica can currently serve for a causal
    /// read — the minimum of the intermediate's applied frontier and the
    /// sub-replica's own confirmed position. Down the chain this is
    /// monotonically non-increasing, so a bookmark read routes to a node only
    /// if that node's visible frontier covers the bookmark's `commit_lsn`.
    ///
    /// Returns `None` for an unknown sub-replica.
    pub fn downstream_visible_frontier(&self, id: &str) -> Option<u64> {
        self.downstream
            .get(id)
            .map(|slot| slot.confirmed_lsn.min(self.self_applied_lsn))
    }

    /// Whether a sub-replica can satisfy a read at `bookmark`. True only when
    /// its visible frontier covers the bookmark's commit LSN.
    pub fn downstream_can_serve(&self, id: &str, bookmark: &CausalBookmark) -> bool {
        self.downstream_visible_frontier(id)
            .is_some_and(|frontier| frontier >= bookmark.commit_lsn())
    }

    /// Select the records to forward to a sub-replica from a batch the
    /// intermediate has on hand.
    ///
    /// `available` is a slice of `(lsn, payload)` the intermediate has
    /// received from its own upstream, assumed ascending by LSN. The result
    /// keeps every record with `requested_since_lsn < lsn <= self_applied_lsn`
    /// — newer than what the sub-replica has, and not beyond what the
    /// intermediate itself holds. Records the intermediate has buffered but
    /// not yet applied are withheld, so a sub-replica never sees data ahead of
    /// its feeder.
    pub fn records_to_forward<'a, T>(
        &self,
        requested_since_lsn: u64,
        available: &'a [(u64, T)],
    ) -> Vec<&'a (u64, T)> {
        let ceiling = self.self_applied_lsn;
        available
            .iter()
            .filter(|(lsn, _)| *lsn > requested_since_lsn && *lsn <= ceiling)
            .collect()
    }
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::election::Member;

    // -- plan_upstream -------------------------------------------------

    #[test]
    fn no_requested_source_streams_from_primary() {
        let (choice, refusal) = plan_upstream("r1", ReplicaClass::AsyncReadReplica, None);
        assert_eq!(choice, UpstreamChoice::Primary);
        assert!(refusal.is_none());
    }

    #[test]
    fn async_read_replica_cascades_from_intermediate() {
        let up = CascadeUpstream::new("inter", "http://inter:55055");
        let (choice, refusal) = plan_upstream("leaf", ReplicaClass::AsyncReadReplica, Some(&up));
        assert!(choice.is_cascade());
        assert_eq!(choice, UpstreamChoice::Intermediate(up));
        assert!(refusal.is_none());
    }

    #[test]
    fn voting_member_refuses_cascade_and_falls_back_to_primary() {
        let up = CascadeUpstream::new("inter", "http://inter:55055");
        let (choice, refusal) = plan_upstream("voter", ReplicaClass::Voting, Some(&up));
        assert_eq!(choice, UpstreamChoice::Primary);
        assert_eq!(refusal, Some(CascadeRefusal::VotingMemberDirectOnly));
    }

    #[test]
    fn node_refuses_to_cascade_from_itself() {
        let up = CascadeUpstream::new("self", "http://self:55055");
        let (choice, refusal) = plan_upstream("self", ReplicaClass::AsyncReadReplica, Some(&up));
        assert_eq!(choice, UpstreamChoice::Primary);
        assert_eq!(refusal, Some(CascadeRefusal::SelfReference));
    }

    #[test]
    fn class_from_member_keeps_voters_direct() {
        assert_eq!(
            ReplicaClass::from_member(&Member::data_voting("v")),
            ReplicaClass::Voting
        );
        assert_eq!(
            ReplicaClass::from_member(&Member::witness("w")),
            ReplicaClass::Voting
        );
        // A non-voting data member is a read-replica candidate.
        assert_eq!(
            ReplicaClass::from_member(&Member::data_catching_up("c")),
            ReplicaClass::AsyncReadReplica
        );
    }

    // -- CascadeRelay --------------------------------------------------

    #[test]
    fn relay_with_no_downstream_reports_own_applied_frontier() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(42);
        assert_eq!(relay.upstream_confirmed_lsn(), 42);
    }

    #[test]
    fn register_downstream_holds_slot_and_is_idempotent() {
        let mut relay = CascadeRelay::new("inter");
        assert_eq!(relay.register_downstream("leaf", 10), 10);
        relay.record_downstream_ack("leaf", 25);
        // Reconnect at an older start must not rewind the live slot.
        assert_eq!(relay.register_downstream("leaf", 5), 25);
        assert_eq!(relay.downstream_count(), 1);
    }

    #[test]
    fn slow_leaf_pins_chain_retention_frontier() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(100);
        relay.register_downstream("fast", 0);
        relay.register_downstream("slow", 0);
        relay.record_downstream_ack("fast", 90);
        relay.record_downstream_ack("slow", 40);
        // The intermediate must keep WAL the slow leaf still needs: the
        // frontier it reports upstream is the slowest leaf, not its own
        // applied position.
        assert_eq!(relay.upstream_confirmed_lsn(), 40);

        // Slow leaf catches up → frontier advances to min(self, fast).
        relay.record_downstream_ack("slow", 95);
        assert_eq!(relay.upstream_confirmed_lsn(), 90);

        // Both pass self_applied is impossible (can't confirm un-forwarded
        // data); clamp holds at self_applied.
        relay.record_downstream_ack("fast", 100);
        relay.record_downstream_ack("slow", 100);
        assert_eq!(relay.upstream_confirmed_lsn(), 100);
    }

    #[test]
    fn releasing_slow_leaf_unblocks_frontier() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(100);
        relay.register_downstream("slow", 0);
        relay.record_downstream_ack("slow", 10);
        assert_eq!(relay.upstream_confirmed_lsn(), 10);
        assert!(relay.unregister_downstream("slow"));
        assert_eq!(relay.upstream_confirmed_lsn(), 100);
        assert!(!relay.unregister_downstream("slow"));
    }

    #[test]
    fn acks_and_forwards_are_monotonic() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(50);
        relay.register_downstream("leaf", 0);
        relay.record_downstream_ack("leaf", 30);
        relay.record_downstream_ack("leaf", 20); // stale, ignored
        relay.note_forwarded("leaf", 45);
        relay.note_forwarded("leaf", 10); // stale, ignored
        let slot = relay.downstream_slot("leaf").unwrap();
        assert_eq!(slot.confirmed_lsn, 30);
        assert_eq!(slot.sent_lsn, 45);
        relay.record_self_applied(20); // stale, ignored
        assert_eq!(relay.self_applied_lsn(), 50);
    }

    #[test]
    fn records_to_forward_bounds_by_since_and_self_applied() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(4);
        let available: Vec<(u64, &str)> =
            vec![(1, "a"), (2, "b"), (3, "c"), (4, "d"), (5, "e"), (6, "f")];
        // since=2 → forward 3,4 (5,6 withheld: not yet applied here).
        let picked = relay.records_to_forward(2, &available);
        let lsns: Vec<u64> = picked.iter().map(|(lsn, _)| *lsn).collect();
        assert_eq!(lsns, vec![3, 4]);

        // After applying more, the ceiling rises.
        relay.record_self_applied(6);
        let picked = relay.records_to_forward(2, &available);
        let lsns: Vec<u64> = picked.iter().map(|(lsn, _)| *lsn).collect();
        assert_eq!(lsns, vec![3, 4, 5, 6]);
    }

    #[test]
    fn visible_frontier_is_monotonically_non_increasing_down_chain() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(80);
        relay.register_downstream("leaf", 0);
        relay.record_downstream_ack("leaf", 60);
        // The leaf can serve up to its own confirmed, never beyond the feeder.
        assert_eq!(relay.downstream_visible_frontier("leaf"), Some(60));

        // If the leaf somehow confirms beyond the feeder (shouldn't happen),
        // the visible frontier still clamps at self_applied.
        relay.record_downstream_ack("leaf", 200);
        assert_eq!(relay.downstream_visible_frontier("leaf"), Some(80));
        assert_eq!(relay.downstream_visible_frontier("unknown"), None);
    }

    #[test]
    fn downstream_can_serve_bookmark_only_when_frontier_covers_it() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(100);
        relay.register_downstream("leaf", 0);
        relay.record_downstream_ack("leaf", 50);
        let within = CausalBookmark::new(1, 50);
        let beyond = CausalBookmark::new(1, 51);
        assert!(relay.downstream_can_serve("leaf", &within));
        assert!(!relay.downstream_can_serve("leaf", &beyond));
        assert!(!relay.downstream_can_serve("missing", &within));
    }

    #[test]
    fn upstream_confirmed_bookmark_stamps_term() {
        let mut relay = CascadeRelay::new("inter");
        relay.record_self_applied(100);
        relay.register_downstream("leaf", 0);
        relay.record_downstream_ack("leaf", 70);
        let bm = relay.upstream_confirmed_bookmark(7);
        assert_eq!(bm.term(), 7);
        assert_eq!(bm.commit_lsn(), 70);
    }
}
