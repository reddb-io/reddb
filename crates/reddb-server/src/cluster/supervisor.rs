//! Member health scoring and automatic range failover (issue #998, PRD #987,
//! ADR 0037).
//!
//! The **Cluster Supervisor** is the control-plane component that watches the
//! authorized members ([`MembershipCatalog`]), decides when an owner has failed,
//! and drives a *safe, fenced* range failover through the one sanctioned path —
//! the ownership transition state machine ([`super::ownership_transition`]). It
//! never edits ownership directly: every promotion it proposes is a
//! [`TransitionRequest`] that the transition machine re-validates (three-part
//! CAS + commit-watermark safety gate) before it touches the catalog.
//!
//! ## Health scoring, not a single short timeout
//!
//! A naive supervisor declares a member dead the instant one heartbeat is late.
//! That is brittle: a single dropped packet, a GC pause, or a brief network
//! hiccup triggers a needless, disruptive failover (and, under load, a *storm*
//! of them). Instead the supervisor combines four signals into a
//! [`HealthScore`]:
//!
//! * **Liveness** — time since the last heartbeat. The dominant signal, but not
//!   the only one.
//! * **Replication lag** — how far behind the range commit watermark the member
//!   is. A live-but-far-behind owner is a poor owner.
//! * **Recent errors** — observed failures in the recent window.
//! * **Grace period** — how long the member has been continuously below the
//!   failover threshold. This is the flapping damper: a member that dips and
//!   recovers inside the grace window is never failed over.
//!
//! The first three combine into a 0..=100 score (weighted, liveness-heavy);
//! the score classifies the member as [`Healthy`](HealthClass::Healthy),
//! [`Degraded`](HealthClass::Degraded), or [`Failed`](HealthClass::Failed). The
//! grace period then gates the *action*: only a `Failed` owner that has stayed
//! failed for at least the grace period is eligible for automatic failover.
//! Together the score and the grace period damp false positives and flapping
//! (acceptance criteria 1 and 4).
//!
//! ## Safe candidate selection
//!
//! When an owner is eligible for failover, the supervisor considers **only**
//! candidates that are (a) current replicas of the range, (b) still authorized
//! data members, and (c) backed by catch-up evidence that covers the range
//! commit watermark — exactly the bar the transition machine enforces. An
//! arbitrary node, a witness, or a replica that has not caught up is never a
//! promotion target (acceptance criterion 2). Among the safe candidates the
//! supervisor prefers the healthiest, breaking ties by stable identity order so
//! the plan is deterministic.
//!
//! The selected promotion is a [`TransitionKind::Promote`] request; activating
//! it bumps the ownership epoch, which fences the failed owner — any write it
//! still attempts under the old epoch is rejected by
//! [`admit_public_write`](super::ownership::ShardOwnershipCatalog::admit_public_write)
//! (acceptance criterion 3).
//!
//! ## Purity
//!
//! All state the supervisor needs from the running cluster — heartbeat,
//! lag, error counts, grace tracking, per-range commit watermarks, and
//! per-candidate catch-up progress — is read through the [`ClusterSignals`]
//! trait, injected by the caller. The supervisor itself is a pure policy over
//! the membership and ownership catalogs plus those signals, so the whole
//! scoring/selection/fencing story is exercised deterministically with a
//! scripted fake — no clock, no network, no engine.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::replication::{
    LivenessObservation, LivenessStatus, MemberHealthInput, ReceivedSignal, SignalPlaneMessage,
};

use super::identity::NodeIdentity;
use super::identity::NodeIdentityError;
use super::membership::MembershipCatalog;
use super::ownership::{CollectionId, RangeId, ShardOwnershipCatalog};
use super::ownership_transition::{
    run_transition, CatchUpEvidence, CommitWatermark, TransitionError, TransitionKind,
    TransitionOutcome, TransitionRequest,
};

/// Raw, point-in-time health signals for one member, read from the running
/// cluster through [`ClusterSignals::member_signals`]. The supervisor turns
/// these into a [`HealthScore`]; it owns no clock or counters itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberSignals {
    /// Time since the member's last heartbeat was received (liveness).
    pub since_last_heartbeat: Duration,
    /// How many WAL LSNs the member trails the range commit watermark by, as an
    /// aggregate liveness-of-replication signal. Zero means fully caught up.
    pub replication_lag_lsn: u64,
    /// Observed errors from the member in the recent observation window.
    pub recent_errors: u32,
    /// How long the member has been *continuously* below the failover
    /// threshold. Zero for a healthy member; the caller resets it the moment the
    /// member recovers. This is the grace-period input that damps flapping —
    /// the supervisor refuses to fail over until it reaches the policy's grace
    /// period.
    pub unhealthy_for: Duration,
}

impl MemberSignals {
    /// A perfectly healthy member: fresh heartbeat, no lag, no errors, never
    /// unhealthy. Handy as a test/observation baseline.
    pub fn healthy() -> Self {
        Self {
            since_last_heartbeat: Duration::ZERO,
            replication_lag_lsn: 0,
            recent_errors: 0,
            unhealthy_for: Duration::ZERO,
        }
    }
}

/// Gossip-derived member health inputs keyed by the same [`NodeIdentity`] the
/// supervisor already scores.
///
/// The tracker is deliberately just a transport adapter: it folds
/// signal-plane liveness and health-input messages into [`MemberSignals`], then
/// the existing [`HealthPolicy`] keeps doing all scoring and grace gating.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GossipMemberSignals {
    members: BTreeMap<NodeIdentity, GossipMemberState>,
}

impl GossipMemberSignals {
    pub fn record_received(
        &mut self,
        observed_at: Duration,
        signal: &ReceivedSignal,
    ) -> Result<(), NodeIdentityError> {
        self.record_message(observed_at, &signal.message)
    }

    pub fn record_message(
        &mut self,
        observed_at: Duration,
        message: &SignalPlaneMessage,
    ) -> Result<(), NodeIdentityError> {
        match message {
            SignalPlaneMessage::LivenessObservation(observation) => {
                let member = NodeIdentity::from_certificate_subject(&observation.observed)?;
                self.members
                    .entry(member)
                    .or_default()
                    .record_liveness(observed_at, observation.status);
            }
            SignalPlaneMessage::MemberHealthInput(input) => {
                let member = NodeIdentity::from_certificate_subject(&input.member)?;
                self.members.entry(member).or_default().record_health(input);
            }
            SignalPlaneMessage::LoadMetricSample(_)
            | SignalPlaneMessage::CatalogVersionHint(_)
            | SignalPlaneMessage::TopologyHint(_) => {}
        }
        Ok(())
    }

    pub fn member_signals_at(
        &self,
        member: &NodeIdentity,
        now: Duration,
        policy: &HealthPolicy,
    ) -> MemberSignals {
        self.members
            .get(member)
            .map(|state| state.to_member_signals(now, policy))
            .unwrap_or_else(MemberSignals::healthy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GossipMemberState {
    liveness: LivenessStatus,
    liveness_since: Duration,
    replication_lag_lsn: u64,
    recent_errors: u32,
}

impl Default for GossipMemberState {
    fn default() -> Self {
        Self {
            liveness: LivenessStatus::Alive,
            liveness_since: Duration::ZERO,
            replication_lag_lsn: 0,
            recent_errors: 0,
        }
    }
}

impl GossipMemberState {
    fn record_liveness(&mut self, observed_at: Duration, liveness: LivenessStatus) {
        if self.liveness != liveness {
            self.liveness = liveness;
            self.liveness_since = observed_at;
        }
    }

    fn record_health(&mut self, input: &MemberHealthInput) {
        self.replication_lag_lsn = input.replication_lag_records;
        self.recent_errors = if input.self_fenced {
            u32::MAX
        } else {
            input.error_count.saturating_add(u32::from(input.read_only))
        };
    }

    fn to_member_signals(&self, now: Duration, policy: &HealthPolicy) -> MemberSignals {
        let unhealthy_for = now.saturating_sub(self.liveness_since);
        let (since_last_heartbeat, unhealthy_for) = match self.liveness {
            LivenessStatus::Alive => (Duration::ZERO, Duration::ZERO),
            LivenessStatus::Suspect => (half_duration(policy.heartbeat_timeout), Duration::ZERO),
            LivenessStatus::Unreachable => (policy.heartbeat_timeout, unhealthy_for),
        };

        MemberSignals {
            since_last_heartbeat,
            replication_lag_lsn: self.replication_lag_lsn,
            recent_errors: self.recent_errors,
            unhealthy_for,
        }
    }
}

fn half_duration(duration: Duration) -> Duration {
    Duration::from_secs_f64(duration.as_secs_f64() / 2.0)
}

/// [`ClusterSignals`] view that gets member health from gossip observations and
/// delegates watermarks/catch-up evidence to the existing control-plane source.
pub struct GossipDerivedSignals<'a, S: ClusterSignals + ?Sized> {
    gossip: &'a GossipMemberSignals,
    delegate: &'a S,
    now: Duration,
    policy: HealthPolicy,
}

impl<'a, S: ClusterSignals + ?Sized> GossipDerivedSignals<'a, S> {
    pub fn new(
        gossip: &'a GossipMemberSignals,
        delegate: &'a S,
        now: Duration,
        policy: HealthPolicy,
    ) -> Self {
        Self {
            gossip,
            delegate,
            now,
            policy,
        }
    }
}

/// How a member's [`HealthScore`] classifies against the policy thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthClass {
    /// Above the degraded threshold — a fully serving member.
    Healthy,
    /// Below the degraded threshold but above the failover threshold — observed
    /// as impaired, but **not** failed over. Surfacing this is what lets an
    /// operator see trouble building before it becomes an outage.
    Degraded,
    /// At or below the failover threshold — a failover candidate *once the grace
    /// period has elapsed*.
    Failed,
}

/// A member's combined health, with the per-axis sub-scores kept visible so an
/// operator (or a test) can see *why* a member scored as it did rather than
/// just the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthScore {
    /// Combined 0..=100 score (higher is healthier).
    pub overall: u8,
    /// Liveness sub-score from the heartbeat age (0..=100).
    pub liveness: u8,
    /// Replication-lag sub-score (0..=100).
    pub lag: u8,
    /// Recent-error sub-score (0..=100).
    pub errors: u8,
    /// The classification the combined score falls into.
    pub class: HealthClass,
}

impl HealthScore {
    pub fn is_healthy(&self) -> bool {
        self.class == HealthClass::Healthy
    }

    pub fn is_failed(&self) -> bool {
        self.class == HealthClass::Failed
    }
}

/// The tunables that turn raw signals into a [`HealthScore`] and gate failover.
///
/// The defaults ([`HealthPolicy::default`]) are deliberately conservative:
/// generous enough that an ordinary hiccup does not trip a failover, with a
/// grace period long enough to ride out a transient blip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    /// Heartbeat age at or beyond which the liveness sub-score bottoms out at 0.
    pub heartbeat_timeout: Duration,
    /// Replication lag (LSNs) at or beyond which the lag sub-score bottoms out.
    pub max_replication_lag: u64,
    /// Recent-error count at or beyond which the error sub-score bottoms out.
    pub max_recent_errors: u32,
    /// Score (inclusive) at or below which a member is [`Failed`](HealthClass::Failed).
    pub failover_threshold: u8,
    /// Score (inclusive) at or below which a member is at least
    /// [`Degraded`](HealthClass::Degraded). Must be `>= failover_threshold`.
    pub degraded_threshold: u8,
    /// How long a member must stay continuously `Failed` before the supervisor
    /// will fail it over. The flapping damper: a shorter blip never triggers a
    /// transition.
    pub grace_period: Duration,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(10),
            max_replication_lag: 10_000,
            max_recent_errors: 20,
            failover_threshold: 30,
            degraded_threshold: 70,
            grace_period: Duration::from_secs(30),
        }
    }
}

/// Linear sub-score in 0..=100: full marks at `0`, zero at/after `limit`.
fn ramp_down(value: f64, limit: f64) -> u8 {
    if limit <= 0.0 {
        // No tolerance configured: anything non-zero is a total failure on this
        // axis, zero is perfect.
        return if value <= 0.0 { 100 } else { 0 };
    }
    let clamped = value.min(limit);
    (100.0 * (1.0 - clamped / limit)).round() as u8
}

impl HealthPolicy {
    /// Combine raw `signals` into a [`HealthScore`] under this policy.
    ///
    /// The three serving signals fold into the overall score with a
    /// liveness-heavy weighting (liveness 70%, lag 20%, errors 10%): a member
    /// whose heartbeat has fully lapsed must be able to reach the failover
    /// threshold on liveness alone — a crashed node stops heartbeating but its
    /// *last-known* lag and error counts may still look fine, so trusting them
    /// would wedge failover shut. At the same time a live owner that is far
    /// behind or erroring is penalised rather than trusted blindly, and a
    /// *brief* heartbeat gap with good lag/errors stays out of the failover band
    /// (which a single short fixed timeout could not express). The grace-period
    /// signal (`unhealthy_for`) is *not* part of the score — it gates the
    /// failover action in [`failover_eligible`](Self::failover_eligible).
    pub fn evaluate(&self, signals: &MemberSignals) -> HealthScore {
        let liveness = ramp_down(
            signals.since_last_heartbeat.as_secs_f64(),
            self.heartbeat_timeout.as_secs_f64(),
        );
        let lag = ramp_down(
            signals.replication_lag_lsn as f64,
            self.max_replication_lag as f64,
        );
        let errors = ramp_down(signals.recent_errors as f64, self.max_recent_errors as f64);

        let overall =
            (liveness as f64 * 0.7 + lag as f64 * 0.2 + errors as f64 * 0.1).round() as u8;
        let class = if overall <= self.failover_threshold {
            HealthClass::Failed
        } else if overall <= self.degraded_threshold {
            HealthClass::Degraded
        } else {
            HealthClass::Healthy
        };

        HealthScore {
            overall,
            liveness,
            lag,
            errors,
            class,
        }
    }

    /// Whether a member with this `score` and these `signals` is eligible for
    /// automatic failover: it must be [`Failed`](HealthClass::Failed) **and**
    /// have stayed unhealthy for at least the grace period. A `Failed` member
    /// still inside the grace window is held back — the flapping damper.
    pub fn failover_eligible(&self, score: &HealthScore, signals: &MemberSignals) -> bool {
        score.is_failed() && signals.unhealthy_for >= self.grace_period
    }
}

/// The cluster state the supervisor reads but does not own: per-member health
/// signals, per-range commit watermarks, and per-candidate catch-up evidence.
///
/// Production backs this onto the heartbeat tracker, the replica registry, and
/// the per-range stream progress (issue #992); tests back it onto a scripted
/// fake. Keeping it behind a trait is what makes the supervisor a pure policy.
pub trait ClusterSignals {
    /// Current raw health signals for `member`.
    fn member_signals(&self, member: &NodeIdentity) -> MemberSignals;

    /// The range commit watermark a promotion candidate must cover for
    /// `(collection, range_id)` — the highest `(term, lsn)` known durable under
    /// the range's commit policy.
    fn commit_watermark(&self, collection: &CollectionId, range_id: RangeId) -> CommitWatermark;

    /// The catch-up evidence the supervisor has for `candidate` on the range, or
    /// `None` if the candidate's progress is unknown (in which case it cannot be
    /// promoted — fail closed).
    fn catch_up(
        &self,
        collection: &CollectionId,
        range_id: RangeId,
        candidate: &NodeIdentity,
    ) -> Option<CatchUpEvidence>;
}

impl<S: ClusterSignals + ?Sized> ClusterSignals for GossipDerivedSignals<'_, S> {
    fn member_signals(&self, member: &NodeIdentity) -> MemberSignals {
        self.gossip
            .member_signals_at(member, self.now, &self.policy)
    }

    fn commit_watermark(&self, collection: &CollectionId, range_id: RangeId) -> CommitWatermark {
        self.delegate.commit_watermark(collection, range_id)
    }

    fn catch_up(
        &self,
        collection: &CollectionId,
        range_id: RangeId,
        candidate: &NodeIdentity,
    ) -> Option<CatchUpEvidence> {
        self.delegate.catch_up(collection, range_id, candidate)
    }
}

/// A safe, validated promotion the supervisor proposes for one range: the failed
/// owner, the chosen caught-up candidate, the owner's health at decision time,
/// and the [`TransitionRequest`] (already carrying the three-part CAS,
/// watermark, and catch-up evidence) to run through the transition machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPromotion {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub failed_owner: NodeIdentity,
    pub candidate: NodeIdentity,
    pub candidate_score: HealthScore,
    pub owner_score: HealthScore,
    pub request: TransitionRequest,
}

/// Why a failing owner's range could **not** be failed over. Surfaced rather
/// than silently skipped, so an operator can see a range that needs attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedReason {
    /// The owner is failing but no replica is a safe candidate — none is an
    /// authorized data member with catch-up evidence covering the commit
    /// watermark. Failing over here could lose committed writes.
    NoSafeCandidate,
}

/// A failing owner's range with no safe failover target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedFailover {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub failed_owner: NodeIdentity,
    pub owner_score: HealthScore,
    pub reason: BlockedReason,
}

/// The supervisor's decision for one scan: the safe promotions to run and the
/// failing ranges with no safe target. A cluster with all owners healthy yields
/// an empty plan ([`is_empty`](Self::is_empty)) — the healthy no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailoverPlan {
    /// Safe, ready-to-run promotions, in `(collection, range_id)` order.
    pub promotions: Vec<PlannedPromotion>,
    /// Failing ranges that have no safe candidate.
    pub blocked: Vec<BlockedFailover>,
}

impl FailoverPlan {
    /// Nothing to do — every owner is healthy (or degraded-but-not-failed, or
    /// within its grace period). The healthy no-op the supervisor must produce
    /// for a stable cluster.
    pub fn is_empty(&self) -> bool {
        self.promotions.is_empty() && self.blocked.is_empty()
    }
}

/// The cluster supervisor: health scoring + automatic range failover planning.
///
/// Holds only the [`HealthPolicy`]; all live state is read through
/// [`ClusterSignals`] at scan time, so one supervisor instance serves the whole
/// cluster lifetime.
#[derive(Debug, Clone, Default)]
pub struct ClusterSupervisor {
    policy: HealthPolicy,
}

impl ClusterSupervisor {
    /// A supervisor with the given health policy.
    pub fn new(policy: HealthPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &HealthPolicy {
        &self.policy
    }

    /// Score a single member's health under the policy. The building block of
    /// degraded-member detection: an operator surface calls this for every
    /// authorized member to render a health view.
    pub fn assess(&self, signals: &MemberSignals) -> HealthScore {
        self.policy.evaluate(signals)
    }

    /// Score every authorized member of `membership`, in stable identity order.
    /// Includes healthy, degraded, and failed members alike — the input to a
    /// cluster health dashboard.
    pub fn assess_members(
        &self,
        membership: &MembershipCatalog,
        signals: &impl ClusterSignals,
    ) -> BTreeMap<NodeIdentity, HealthScore> {
        membership
            .members()
            .map(|m| {
                let id = m.identity().clone();
                let score = self.policy.evaluate(&signals.member_signals(&id));
                (id, score)
            })
            .collect()
    }

    /// Plan automatic failovers across the whole ownership catalog **without**
    /// mutating it. For each range whose owner is failover-eligible (Failed and
    /// past the grace period), pick the safest caught-up replica candidate and
    /// produce a [`PlannedPromotion`]; if no replica is safe, record a
    /// [`BlockedFailover`]. Owners that are healthy, merely degraded, or still
    /// inside their grace period produce nothing.
    pub fn plan_failovers(
        &self,
        membership: &MembershipCatalog,
        ownership: &ShardOwnershipCatalog,
        signals: &impl ClusterSignals,
    ) -> FailoverPlan {
        // entries() yields ranges in (collection, range_id) order, so the plan
        // is deterministic.
        let mut plan = FailoverPlan::default();

        for range in ownership.entries() {
            let owner = range.owner().clone();
            let owner_signals = signals.member_signals(&owner);
            let owner_score = self.policy.evaluate(&owner_signals);

            // Healthy or degraded-but-not-failed owners are left alone; a failed
            // owner still inside its grace period is held back (flapping damper).
            if !self.policy.failover_eligible(&owner_score, &owner_signals) {
                continue;
            }

            let collection = range.collection().clone();
            let range_id = range.range_id();
            let watermark = signals.commit_watermark(&collection, range_id);

            // Consider only safe candidates: a current replica, still an
            // authorized data member, not itself failed, with catch-up evidence
            // covering the commit watermark.
            let mut best: Option<(HealthScore, CatchUpEvidence, NodeIdentity)> = None;
            for candidate in range.replicas() {
                if !membership
                    .member(candidate)
                    .is_some_and(|m| m.kind().holds_data())
                {
                    continue;
                }
                let cand_score = self.policy.evaluate(&signals.member_signals(candidate));
                if cand_score.is_failed() {
                    // Promoting a failed replica just moves the outage; skip it.
                    continue;
                }
                let Some(evidence) = signals.catch_up(&collection, range_id, candidate) else {
                    continue;
                };
                if !evidence.covers(watermark) {
                    // Replica is a copy of the range but has not caught up to the
                    // commit watermark — promoting it could lose committed
                    // writes. This is the unsafe-candidate rejection.
                    continue;
                }

                // Prefer the healthiest candidate; break ties by stable identity
                // order for determinism.
                let better = match &best {
                    None => true,
                    Some((best_score, _, best_id)) => {
                        cand_score.overall > best_score.overall
                            || (cand_score.overall == best_score.overall && candidate < best_id)
                    }
                };
                if better {
                    best = Some((cand_score, evidence, candidate.clone()));
                }
            }

            match best {
                Some((candidate_score, evidence, candidate)) => {
                    let request = TransitionRequest::new(
                        TransitionKind::Promote,
                        collection.clone(),
                        range_id,
                        owner.clone(),
                        range.epoch(),
                        range.version(),
                        candidate.clone(),
                        watermark,
                    )
                    .with_evidence(evidence)
                    .with_replicas(remaining_replicas(range.replicas(), &candidate));
                    plan.promotions.push(PlannedPromotion {
                        collection,
                        range_id,
                        failed_owner: owner,
                        candidate,
                        candidate_score,
                        owner_score,
                        request,
                    });
                }
                None => plan.blocked.push(BlockedFailover {
                    collection,
                    range_id,
                    failed_owner: owner,
                    owner_score,
                    reason: BlockedReason::NoSafeCandidate,
                }),
            }
        }

        plan
    }

    /// Plan failovers and immediately run the safe promotions through the
    /// ownership transition machine, fencing each failed owner via the epoch
    /// bump. Returns the activated [`TransitionOutcome`]s and the surviving
    /// [`FailoverPlan`] (whose `blocked` entries still need attention; its
    /// `promotions` are the requests that were run).
    ///
    /// Each promotion is an independent catalog entry, so running them in
    /// sequence never invalidates another's CAS. A promotion whose CAS lost a
    /// race (the catalog moved between planning and activation) surfaces as a
    /// [`TransitionError`] in the returned vector rather than aborting the rest.
    pub fn run_failovers(
        &self,
        membership: &MembershipCatalog,
        ownership: &mut ShardOwnershipCatalog,
        signals: &impl ClusterSignals,
    ) -> (
        Vec<Result<TransitionOutcome, TransitionError>>,
        FailoverPlan,
    ) {
        let plan = self.plan_failovers(membership, ownership, signals);
        let outcomes = plan
            .promotions
            .iter()
            .map(|p| run_transition(ownership, &p.request))
            .collect();
        (outcomes, plan)
    }
}

/// The replica set the new owner carries after promotion: the old replica set
/// minus the promoted candidate (it becomes owner, not its own replica). The
/// failed owner is intentionally *not* added back as a replica — it is fenced
/// and presumed down; the rebalancer re-replicates once it returns or is
/// replaced.
fn remaining_replicas(replicas: &[NodeIdentity], promoted: &NodeIdentity) -> Vec<NodeIdentity> {
    replicas
        .iter()
        .filter(|r| *r != promoted)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::membership::{ClusterId, ClusterMember, MemberKind};
    use crate::cluster::ownership::{CatalogVersion, OwnershipEpoch};
    use crate::cluster::ownership::{
        PlacementMetadata, RangeBounds, RangeOwnership, RangeRole, RangeWriteReject, ShardKeyMode,
    };
    use std::collections::HashMap;

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn data_member(cn: &str) -> ClusterMember {
        ClusterMember::joined_empty(ident(cn), MemberKind::Data)
    }

    fn membership(members: &[&str]) -> MembershipCatalog {
        MembershipCatalog::new(
            ClusterId::new("cluster-x").unwrap(),
            members.iter().map(|m| data_member(m)),
        )
    }

    /// A catalog with one full-keyspace range `orders/1` owned by `owner` with
    /// `replicas`, at the initial epoch/version.
    fn catalog_with(owner: &str, replicas: &[&str]) -> (ShardOwnershipCatalog, CollectionId) {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .apply_update(RangeOwnership::establish(
                orders.clone(),
                RangeId::new(1),
                ShardKeyMode::Hash,
                RangeBounds::full(),
                ident(owner),
                replicas.iter().map(|r| ident(r)).collect::<Vec<_>>(),
                PlacementMetadata::with_replication_factor(3),
            ))
            .unwrap();
        (catalog, orders)
    }

    /// A scripted [`ClusterSignals`]: per-member signals, one shared watermark,
    /// and per-(range,candidate) catch-up evidence keyed by candidate CN.
    struct FakeSignals {
        members: HashMap<NodeIdentity, MemberSignals>,
        watermark: CommitWatermark,
        catch_up: HashMap<NodeIdentity, CatchUpEvidence>,
    }

    impl FakeSignals {
        fn new(watermark: CommitWatermark) -> Self {
            Self {
                members: HashMap::new(),
                watermark,
                catch_up: HashMap::new(),
            }
        }

        fn with_member(mut self, cn: &str, signals: MemberSignals) -> Self {
            self.members.insert(ident(cn), signals);
            self
        }

        fn with_catch_up(mut self, cn: &str, applied_term: u64, applied_lsn: u64) -> Self {
            self.catch_up.insert(
                ident(cn),
                CatchUpEvidence::new(ident(cn), applied_term, applied_lsn),
            );
            self
        }
    }

    impl ClusterSignals for FakeSignals {
        fn member_signals(&self, member: &NodeIdentity) -> MemberSignals {
            self.members
                .get(member)
                .copied()
                .unwrap_or_else(MemberSignals::healthy)
        }

        fn commit_watermark(
            &self,
            _collection: &CollectionId,
            _range_id: RangeId,
        ) -> CommitWatermark {
            self.watermark
        }

        fn catch_up(
            &self,
            _collection: &CollectionId,
            _range_id: RangeId,
            candidate: &NodeIdentity,
        ) -> Option<CatchUpEvidence> {
            self.catch_up.get(candidate).cloned()
        }
    }

    /// Signals for a failed-and-past-grace owner: no heartbeat for a long time,
    /// well over the default grace period.
    fn failed_signals() -> MemberSignals {
        MemberSignals {
            since_last_heartbeat: Duration::from_secs(60),
            replication_lag_lsn: 50_000,
            recent_errors: 100,
            unhealthy_for: Duration::from_secs(60),
        }
    }

    // --- health scoring ---------------------------------------------------

    #[test]
    fn fresh_member_scores_perfectly_healthy() {
        let policy = HealthPolicy::default();
        let score = policy.evaluate(&MemberSignals::healthy());
        assert_eq!(score.overall, 100);
        assert_eq!(score.class, HealthClass::Healthy);
    }

    #[test]
    fn score_combines_signals_not_just_a_timeout() {
        // A member with a *brief* heartbeat gap but good lag and no errors should
        // not be treated as dead the way a short fixed timeout would. Its liveness
        // sub-score dips, but lag/errors keep the overall in the Healthy band.
        let policy = HealthPolicy::default();
        let signals = MemberSignals {
            since_last_heartbeat: Duration::from_secs(2), // 1/5 of the 10s timeout
            replication_lag_lsn: 0,
            recent_errors: 0,
            unhealthy_for: Duration::ZERO,
        };
        let score = policy.evaluate(&signals);
        assert_eq!(score.liveness, 80, "heartbeat at 1/5 of the timeout");
        assert_eq!(score.lag, 100);
        assert_eq!(score.errors, 100);
        // overall = 0.7*80 + 0.2*100 + 0.1*100 = 56 + 20 + 10 = 86 -> Healthy.
        // A 2s fixed timeout would have declared this member dead.
        assert_eq!(score.overall, 86);
        assert_eq!(score.class, HealthClass::Healthy);
    }

    #[test]
    fn lag_and_errors_pull_a_live_member_into_degraded() {
        // A member that is heartbeating fine but far behind and erroring is
        // penalised — a single timeout would have called it perfectly healthy.
        let policy = HealthPolicy::default();
        let signals = MemberSignals {
            since_last_heartbeat: Duration::ZERO,
            replication_lag_lsn: 10_000, // at the cap -> lag sub-score 0
            recent_errors: 20,           // at the cap -> error sub-score 0
            unhealthy_for: Duration::ZERO,
        };
        let score = policy.evaluate(&signals);
        // overall = 0.7*100 + 0.2*0 + 0.1*0 = 70 -> Degraded (<= degraded threshold).
        assert_eq!(score.overall, 70);
        assert_eq!(score.class, HealthClass::Degraded);
    }

    #[test]
    fn dead_heartbeat_alone_reaches_failed() {
        // A crashed node stops heartbeating; even if its last-known lag/errors
        // look perfect, liveness alone must carry it to the failover band — else
        // the most common failure (a clean crash) would never fail over.
        let policy = HealthPolicy::default();
        let signals = MemberSignals {
            since_last_heartbeat: Duration::from_secs(30),
            replication_lag_lsn: 0,
            recent_errors: 0,
            unhealthy_for: Duration::from_secs(30),
        };
        let score = policy.evaluate(&signals);
        assert_eq!(score.liveness, 0);
        // overall = 0.7*0 + 0.2*100 + 0.1*100 = 30 -> Failed (<= failover threshold).
        assert_eq!(score.overall, 30);
        assert_eq!(score.class, HealthClass::Failed);
    }

    #[test]
    fn totally_unreachable_member_is_failed() {
        // A member we cannot reach reports a dead heartbeat *and* growing lag and
        // errors — every axis bottoms out, so it lands well under the failover
        // threshold.
        let policy = HealthPolicy::default();
        let signals = MemberSignals {
            since_last_heartbeat: Duration::from_secs(30),
            replication_lag_lsn: 50_000,
            recent_errors: 100,
            unhealthy_for: Duration::from_secs(30),
        };
        let score = policy.evaluate(&signals);
        assert_eq!(score.overall, 0);
        assert_eq!(score.class, HealthClass::Failed);
    }

    // --- failover planning: the five acceptance scenarios -----------------

    #[test]
    fn healthy_cluster_is_a_no_op() {
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        // All members healthy by default.
        let signals = FakeSignals::new(CommitWatermark::new(1, 10));

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert!(plan.is_empty(), "no failover when every owner is healthy");
    }

    #[test]
    fn degraded_owner_is_detected_but_not_failed_over() {
        // node-a is degraded (live, but lagging+erroring) — observable, but the
        // supervisor must not move ownership for a merely-degraded owner.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10)).with_member(
            "CN=node-a",
            MemberSignals {
                since_last_heartbeat: Duration::ZERO,
                replication_lag_lsn: 10_000,
                recent_errors: 20,
                unhealthy_for: Duration::ZERO,
            },
        );

        let score = supervisor.assess(&signals.member_signals(&ident("CN=node-a")));
        assert_eq!(score.class, HealthClass::Degraded, "detected as degraded");

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert!(plan.is_empty(), "a degraded owner is not failed over");
    }

    #[test]
    fn safe_candidate_is_promoted_and_old_owner_is_fenced() {
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals())
            // node-b is healthy and fully caught up; node-c is caught up too but
            // we expect node-b chosen on identity tie-break.
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        let (outcomes, plan) = supervisor.run_failovers(&members, &mut catalog, &signals);
        assert_eq!(plan.promotions.len(), 1);
        assert!(plan.blocked.is_empty());
        let promotion = &plan.promotions[0];
        assert_eq!(promotion.failed_owner, ident("CN=node-a"));
        assert_eq!(
            promotion.candidate,
            ident("CN=node-b"),
            "healthiest, tie -> lowest id"
        );

        let outcome = outcomes[0].as_ref().expect("promotion should activate");
        assert_eq!(outcome.kind, TransitionKind::Promote);
        assert!(
            outcome.fenced_old_owner(),
            "epoch bumped to fence old owner"
        );
        assert_eq!(outcome.new_owner, ident("CN=node-b"));

        // The catalog now makes node-b the owner at the bumped epoch, and the old
        // owner node-a is fenced from public writes under the old epoch.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-b"));
        assert_eq!(range.role_of(&ident("CN=node-b")), RangeRole::Owner);
        let err = catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            RangeWriteReject::NotOwner { .. } | RangeWriteReject::StaleEpoch { .. }
        ));
    }

    #[test]
    fn unsafe_candidate_behind_watermark_is_rejected() {
        // node-a failed; its only replica node-b is a copy of the range but has
        // NOT caught up to the commit watermark (term 2 lsn 50 vs applied 2/49).
        // Promoting it could lose committed writes, so failover is blocked and
        // the catalog is untouched.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(2, 50))
            .with_member("CN=node-a", failed_signals())
            .with_catch_up("CN=node-b", 2, 49); // one LSN short

        let (outcomes, plan) = supervisor.run_failovers(&members, &mut catalog, &signals);
        assert!(plan.promotions.is_empty(), "no safe promotion");
        assert!(outcomes.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockedReason::NoSafeCandidate);
        assert_eq!(plan.blocked[0].failed_owner, ident("CN=node-a"));

        // Catalog is unchanged — node-a still owner at the initial epoch.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.epoch(), OwnershipEpoch::initial());
        assert_eq!(range.version(), CatalogVersion::initial());
    }

    #[test]
    fn flapping_owner_within_grace_period_is_not_failed_over() {
        // node-a's score is Failed, but it has only been unhealthy for 2s — well
        // inside the default 30s grace period. A flap must not move ownership.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member(
                "CN=node-a",
                MemberSignals {
                    since_last_heartbeat: Duration::from_secs(30),
                    replication_lag_lsn: 50_000,
                    recent_errors: 100,
                    unhealthy_for: Duration::from_secs(2), // inside grace
                },
            )
            .with_catch_up("CN=node-b", 1, 10);

        // The owner *is* scored Failed...
        let score = supervisor.assess(&signals.member_signals(&ident("CN=node-a")));
        assert_eq!(score.class, HealthClass::Failed);
        // ...but the grace period holds the failover back.
        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert!(plan.is_empty(), "flap inside grace period is damped");
    }

    #[test]
    fn unknown_candidate_progress_blocks_failover() {
        // node-a failed; node-b is a replica and a member, but the supervisor has
        // no catch-up evidence for it. Fail closed: blocked, not promoted.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals());

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockedReason::NoSafeCandidate);
    }

    #[test]
    fn non_replica_node_is_never_a_candidate() {
        // node-a failed and has NO replicas for the range. node-c is a healthy,
        // caught-up member — but it is not a replica, so it is never considered.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-c"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &[]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals())
            .with_catch_up("CN=node-c", 9, 999);

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert_eq!(plan.blocked.len(), 1, "no replica -> no safe candidate");
        assert!(plan.promotions.is_empty());
    }

    #[test]
    fn failed_replica_is_not_promoted() {
        // node-a failed; node-b is a caught-up replica but is itself failed.
        // Promoting it would just move the outage, so it is not selected.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals())
            .with_member("CN=node-b", failed_signals())
            .with_catch_up("CN=node-b", 1, 10);

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockedReason::NoSafeCandidate);
    }

    #[test]
    fn healthiest_caught_up_candidate_is_preferred() {
        // Both replicas are caught up, but node-c is healthier than node-b, so it
        // wins despite node-b sorting first by identity.
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals())
            .with_member(
                "CN=node-b",
                MemberSignals {
                    since_last_heartbeat: Duration::from_secs(4),
                    replication_lag_lsn: 0,
                    recent_errors: 0,
                    unhealthy_for: Duration::ZERO,
                },
            ) // degraded-ish liveness, lower score
            .with_member("CN=node-c", MemberSignals::healthy())
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        let plan = supervisor.plan_failovers(&members, &catalog, &signals);
        assert_eq!(plan.promotions.len(), 1);
        assert_eq!(
            plan.promotions[0].candidate,
            ident("CN=node-c"),
            "healthier candidate preferred over identity tie-break",
        );
    }

    #[test]
    fn promoted_owner_drops_itself_from_the_replica_set() {
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals())
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        supervisor.run_failovers(&members, &mut catalog, &signals);
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-b"));
        // node-b is no longer in its own replica set; node-c remains; the fenced
        // old owner node-a is not re-added.
        assert!(!range.replicas().contains(&ident("CN=node-b")));
        assert!(range.replicas().contains(&ident("CN=node-c")));
        assert!(!range.replicas().contains(&ident("CN=node-a")));
    }

    #[test]
    fn assess_members_scores_every_authorized_member() {
        let supervisor = ClusterSupervisor::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_member("CN=node-a", failed_signals());

        let scores = supervisor.assess_members(&members, &signals);
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[&ident("CN=node-a")].class, HealthClass::Failed);
        assert_eq!(scores[&ident("CN=node-b")].class, HealthClass::Healthy);
    }

    #[test]
    fn gossip_alive_observation_scores_member_healthy_without_direct_heartbeat() {
        let policy = HealthPolicy::default();
        let member = ident("CN=node-a");
        let mut gossip = GossipMemberSignals::default();
        gossip
            .record_message(
                Duration::ZERO,
                &SignalPlaneMessage::LivenessObservation(LivenessObservation {
                    observer: "CN=node-c".to_string(),
                    observed: "CN=node-a".to_string(),
                    incarnation: 1,
                    status: LivenessStatus::Alive,
                }),
            )
            .unwrap();
        gossip
            .record_message(
                Duration::ZERO,
                &SignalPlaneMessage::MemberHealthInput(MemberHealthInput {
                    member: "CN=node-a".to_string(),
                    error_count: 0,
                    replication_lag_records: 0,
                    read_only: false,
                    self_fenced: false,
                }),
            )
            .unwrap();

        let signals = gossip.member_signals_at(&member, Duration::from_secs(60), &policy);
        let score = policy.evaluate(&signals);

        assert_eq!(signals, MemberSignals::healthy());
        assert_eq!(score.class, HealthClass::Healthy);
    }

    #[test]
    fn gossip_unreachable_observation_crosses_grace_and_triggers_failover() {
        let policy = HealthPolicy {
            grace_period: Duration::from_secs(5),
            ..HealthPolicy::default()
        };
        let supervisor = ClusterSupervisor::new(policy);
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let base_signals =
            FakeSignals::new(CommitWatermark::new(1, 10)).with_catch_up("CN=node-b", 1, 10);
        let mut gossip = GossipMemberSignals::default();
        gossip
            .record_message(
                Duration::ZERO,
                &SignalPlaneMessage::LivenessObservation(LivenessObservation {
                    observer: "CN=node-b".to_string(),
                    observed: "CN=node-a".to_string(),
                    incarnation: 2,
                    status: LivenessStatus::Unreachable,
                }),
            )
            .unwrap();
        gossip
            .record_message(
                Duration::ZERO,
                &SignalPlaneMessage::MemberHealthInput(MemberHealthInput {
                    member: "CN=node-a".to_string(),
                    error_count: 100,
                    replication_lag_records: 50_000,
                    read_only: false,
                    self_fenced: false,
                }),
            )
            .unwrap();
        let signals =
            GossipDerivedSignals::new(&gossip, &base_signals, policy.grace_period, policy);

        let (outcomes, plan) = supervisor.run_failovers(&members, &mut catalog, &signals);

        assert_eq!(plan.promotions.len(), 1);
        assert_eq!(plan.promotions[0].failed_owner, ident("CN=node-a"));
        assert_eq!(plan.promotions[0].candidate, ident("CN=node-b"));
        assert!(outcomes[0].as_ref().unwrap().fenced_old_owner());
    }
}
