//! Cluster drain and force-remove flows (issue #1000, PRD #987, ADR 0037).
//!
//! Removing a data member from a multi-writer cluster is not a single catalog
//! edit — a member may own ranges (it is the sole writer) and replicate others (it
//! holds required copies). Dropping it from membership while a range still depends
//! on it would orphan write authority or lose a copy. This module is the policy
//! that moves those dependencies off a member *first*, in the two shapes the
//! glossary names:
//!
//! * **Cluster drain** — the *planned* removal of a live, cooperating member. The
//!   member is marked [`Draining`](super::membership::MemberState::Draining) so it
//!   stops receiving new range placements, then every range it owns is handed off
//!   to a caught-up replica through the ordinary fenced transition machine
//!   ([`super::ownership_transition`]) and every range it replicates is evacuated
//!   to another member. Only once it owns and replicates nothing is membership
//!   removal allowed — a removal that still has a dependency is *refused*, not
//!   forced.
//! * **Force remove** — the *unplanned* removal of a dead or unrecoverable member.
//!   The member cannot cooperate (it is gone), so ordinary safety checks cannot be
//!   satisfied. Under the ADR 0037 forced-ownership rules a `FORCE` order — a
//!   special administrative capability plus an explicit operator reason — promotes
//!   the most-caught-up surviving replica even when it cannot prove it covers the
//!   commit watermark, recording the possible committed-write loss as durable
//!   audit evidence, and bumps the ownership epoch so the dead owner is fenced if
//!   it ever reappears. A range with no surviving replica at all is surfaced as
//!   *unrecoverable* rather than silently dropped.
//!
//! ## Purity
//!
//! Like the supervisor ([`super::supervisor`]), everything here is a pure policy
//! over the membership and ownership catalogs plus the [`ClusterSignals`] the
//! caller injects (per-range commit watermarks and per-candidate catch-up
//! evidence). There is no clock, network, or engine, so the whole drain /
//! force-remove / refusal / audit story is exercised deterministically.

use super::identity::NodeIdentity;
use super::membership::{ClusterMember, MembershipCatalog};
use super::ownership::{CollectionId, RangeId, RangeOwnership, ShardOwnershipCatalog};
use super::ownership_transition::{
    run_transition, CatchUpEvidence, CommitWatermark, TransitionError, TransitionKind,
    TransitionOutcome, TransitionRequest,
};
use super::supervisor::ClusterSignals;

// =============================================================================
// Planned drain
// =============================================================================

/// One scheduled step that moves a single range's dependency off the draining
/// member. A complete [`DrainPlan`] is a list of these plus any [`DrainBlock`]s
/// that could not be scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainStep {
    /// The draining member *owns* this range: hand write authority off to a
    /// caught-up replica through a fenced [`Handoff`](TransitionKind::Handoff)
    /// transition.
    Handoff(OwnedHandoff),
    /// The draining member *replicates* this range: drop its copy from the owner's
    /// replica set, moving the copy to a `replacement` member if one is needed to
    /// keep the range at its replication factor.
    Evacuate(ReplicaEvacuation),
}

/// A scheduled hand-off of an owned range away from the draining member to a
/// safe, caught-up replica. The [`TransitionRequest`] already carries the
/// three-part CAS, the commit watermark, and the target's catch-up evidence, so
/// it runs through [`run_transition`] unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedHandoff {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub target: NodeIdentity,
    pub request: TransitionRequest,
}

/// A scheduled evacuation of the draining member's *replica* copy of a range.
/// `replacement` is `Some` when a new host was assigned to preserve the range's
/// replication factor, or `None` when the range is already replicated enough to
/// drop the copy outright. `next` is the catalog entry the evacuation installs
/// (a replica-set change — no epoch bump, since write authority does not move).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaEvacuation {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub replacement: Option<NodeIdentity>,
    pub next: RangeOwnership,
}

/// Why a range could not be scheduled off the draining member. Surfaced rather
/// than silently skipped, so an operator sees exactly which range is holding the
/// drain open (and removal blocked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainBlockReason {
    /// An *owned* range has no safe hand-off target — no replica is an active data
    /// member with catch-up evidence covering the commit watermark. Handing off to
    /// a node that has not caught up could lose committed writes.
    NoSafeHandoffTarget,
    /// A *replicated* range cannot shed the draining member's copy without dropping
    /// below its replication factor, and no eligible member is free to host a
    /// replacement copy.
    NoReplacementReplica,
}

impl DrainBlockReason {
    fn label(self) -> &'static str {
        match self {
            DrainBlockReason::NoSafeHandoffTarget => {
                "no caught-up replica is a safe hand-off target"
            }
            DrainBlockReason::NoReplacementReplica => {
                "no eligible member can host a replacement replica"
            }
        }
    }
}

/// A range that blocks the drain: it still depends on the draining member and
/// could not be moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainBlock {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub reason: DrainBlockReason,
}

/// The planned drain of one member: the steps that move its ranges off it, and
/// the ranges that could not be moved. A member that owns and replicates nothing
/// yields an empty plan ([`is_empty`](Self::is_empty)); a plan with no
/// [`blocked`](Self::blocked) entries is [`complete`](Self::is_complete) and the
/// member becomes removable once the steps are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainPlan {
    pub member: NodeIdentity,
    pub steps: Vec<DrainStep>,
    pub blocked: Vec<DrainBlock>,
}

impl DrainPlan {
    /// Nothing to move and nothing blocked — the member already holds no ranges.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.blocked.is_empty()
    }

    /// Every dependency could be scheduled (no [`blocked`](Self::blocked) ranges),
    /// so applying the steps fully drains the member and removal will be allowed.
    pub fn is_complete(&self) -> bool {
        self.blocked.is_empty()
    }
}

/// Plan a member's drain **without** mutating either catalog. For every range the
/// member owns, schedule a fenced hand-off to the safest caught-up replica; for
/// every range it replicates, schedule an evacuation (with a replacement host
/// when the replication factor requires one). Ranges that cannot be moved become
/// [`DrainBlock`]s.
pub fn plan_drain(
    member: &NodeIdentity,
    membership: &MembershipCatalog,
    ownership: &ShardOwnershipCatalog,
    signals: &impl ClusterSignals,
) -> DrainPlan {
    let mut steps = Vec::new();
    let mut blocked = Vec::new();

    for range in ownership.entries() {
        let collection = range.collection().clone();
        let range_id = range.range_id();

        if range.owner() == member {
            // Owned range: hand authority off to a caught-up replica.
            let watermark = signals.commit_watermark(&collection, range_id);
            match select_handoff_target(range, member, membership, watermark, signals) {
                Some((target, evidence)) => {
                    let request = TransitionRequest::new(
                        TransitionKind::Handoff,
                        collection.clone(),
                        range_id,
                        member.clone(),
                        range.epoch(),
                        range.version(),
                        target.clone(),
                        watermark,
                    )
                    .with_evidence(evidence)
                    .with_replicas(without(range.replicas(), &target));
                    steps.push(DrainStep::Handoff(OwnedHandoff {
                        collection,
                        range_id,
                        target,
                        request,
                    }));
                }
                None => blocked.push(DrainBlock {
                    collection,
                    range_id,
                    reason: DrainBlockReason::NoSafeHandoffTarget,
                }),
            }
        } else if range.replicas().contains(member) {
            // Replicated range: drop the member's copy, adding a replacement host
            // if dropping it would take the range below its replication factor.
            let remaining = without(range.replicas(), member);
            // copies after dropping = owner (1) + remaining replicas.
            let copies_after = 1 + remaining.len();
            let required = range.placement().replication_factor();
            if copies_after >= required {
                let next = range.update_replicas(remaining);
                steps.push(DrainStep::Evacuate(ReplicaEvacuation {
                    collection,
                    range_id,
                    replacement: None,
                    next,
                }));
            } else if let Some(replacement) = select_replacement_replica(range, member, membership)
            {
                let mut replicas = remaining;
                replicas.push(replacement.clone());
                let next = range.update_replicas(replicas);
                steps.push(DrainStep::Evacuate(ReplicaEvacuation {
                    collection,
                    range_id,
                    replacement: Some(replacement),
                    next,
                }));
            } else {
                blocked.push(DrainBlock {
                    collection,
                    range_id,
                    reason: DrainBlockReason::NoReplacementReplica,
                });
            }
        }
    }

    DrainPlan {
        member: member.clone(),
        steps,
        blocked,
    }
}

/// The outcome of running a drain: the result of each scheduled step (a hand-off
/// outcome or an evacuation), and the ranges that stayed blocked. The membership
/// catalog is untouched — removal is the separate, gated
/// [`commit_drain_removal`] step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    pub member: NodeIdentity,
    /// Each owned-range hand-off's transition result, in plan order.
    pub handoffs: Vec<Result<TransitionOutcome, TransitionError>>,
    /// Each replica evacuation that was applied, in plan order.
    pub evacuations: Vec<ReplicaEvacuation>,
    /// Ranges that could not be scheduled and still block the drain.
    pub blocked: Vec<DrainBlock>,
}

impl DrainOutcome {
    /// Did every scheduled step apply cleanly and nothing stay blocked? When true,
    /// the member now owns and replicates nothing and [`commit_drain_removal`]
    /// will succeed.
    pub fn is_drained(&self) -> bool {
        self.blocked.is_empty() && self.handoffs.iter().all(Result::is_ok)
    }
}

/// Plan a drain and apply its steps to the ownership catalog. Owned-range
/// hand-offs run through the fenced transition machine; replica evacuations are
/// applied as replica-set updates. Membership is **not** changed here — finishing
/// the removal is the gated [`commit_drain_removal`].
pub fn run_drain(
    member: &NodeIdentity,
    membership: &MembershipCatalog,
    ownership: &mut ShardOwnershipCatalog,
    signals: &impl ClusterSignals,
) -> DrainOutcome {
    let plan = plan_drain(member, membership, ownership, signals);
    let mut handoffs = Vec::new();
    let mut evacuations = Vec::new();

    for step in plan.steps {
        match step {
            DrainStep::Handoff(handoff) => {
                handoffs.push(run_transition(ownership, &handoff.request));
            }
            DrainStep::Evacuate(evac) => {
                // A replica-set update strictly advances the entry version, so it
                // cannot fail the catalog's monotonicity check; surface any
                // catalog error by leaving the catalog untouched is unnecessary —
                // apply and record.
                if ownership.apply_update(evac.next.clone()).is_ok() {
                    evacuations.push(evac);
                }
            }
        }
    }

    DrainOutcome {
        member: member.clone(),
        handoffs,
        evacuations,
        blocked: plan.blocked,
    }
}

/// Why a membership removal was refused. Every variant leaves both catalogs
/// untouched — a planned removal fails closed while any dependency remains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalRejection {
    /// The node is not a member of this cluster.
    NotAMember { member: NodeIdentity },
    /// The member has not been marked draining. Planned removal must mark a member
    /// [`Draining`](super::membership::MemberState::Draining) first (use
    /// [`MembershipCatalog::begin_drain`]); force-remove is the path for a member
    /// that was never drained.
    NotDraining { member: NodeIdentity },
    /// The member still owns these ranges — removing it would orphan their write
    /// authority. Drain must hand them off first.
    StillOwnsRanges {
        member: NodeIdentity,
        ranges: Vec<(CollectionId, RangeId)>,
    },
    /// The member still holds replica copies of these ranges — removing it would
    /// drop required copies. Drain must evacuate them first.
    StillReplicaFor {
        member: NodeIdentity,
        ranges: Vec<(CollectionId, RangeId)>,
    },
}

impl std::fmt::Display for RemovalRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAMember { member } => write!(f, "{member} is not a cluster member"),
            Self::NotDraining { member } => {
                write!(f, "{member} must be marked draining before planned removal")
            }
            Self::StillOwnsRanges { member, ranges } => write!(
                f,
                "{member} cannot be removed: still owns {} range(s)",
                ranges.len()
            ),
            Self::StillReplicaFor { member, ranges } => write!(
                f,
                "{member} cannot be removed: still replicates {} range(s)",
                ranges.len()
            ),
        }
    }
}

impl std::error::Error for RemovalRejection {}

/// Finish a planned drain by removing the member from the catalog — but only if
/// it is a draining member that no longer owns or replicates any range. Refuses
/// (leaving membership untouched) on any remaining dependency, so a member is
/// never removed out from under a range that still needs it.
pub fn commit_drain_removal(
    member: &NodeIdentity,
    membership: &mut MembershipCatalog,
    ownership: &ShardOwnershipCatalog,
) -> Result<ClusterMember, RemovalRejection> {
    match membership.member(member) {
        None => {
            return Err(RemovalRejection::NotAMember {
                member: member.clone(),
            })
        }
        Some(m) if !m.is_draining() => {
            return Err(RemovalRejection::NotDraining {
                member: member.clone(),
            })
        }
        Some(_) => {}
    }

    let (owned, replicated) = range_dependencies(member, ownership);
    if !owned.is_empty() {
        return Err(RemovalRejection::StillOwnsRanges {
            member: member.clone(),
            ranges: owned,
        });
    }
    if !replicated.is_empty() {
        return Err(RemovalRejection::StillReplicaFor {
            member: member.clone(),
            ranges: replicated,
        });
    }

    Ok(membership
        .remove(member)
        .expect("membership presence checked above"))
}

// =============================================================================
// Force remove (dead / unrecoverable member)
// =============================================================================

/// Proof that the caller holds the special administrative `FORCE` capability that
/// ADR 0037 requires for forced ownership transitions. Constructing one *is* the
/// capability check at the call boundary; the policy here only proceeds when given
/// one, so a forced removal can never be requested without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceCapability {
    holder: String,
}

impl ForceCapability {
    /// Mint a capability for `holder` (an operator/identity the audit trail
    /// records as the authority behind the forced removal).
    pub fn granted(holder: impl Into<String>) -> Self {
        Self {
            holder: holder.into(),
        }
    }

    pub fn holder(&self) -> &str {
        &self.holder
    }
}

/// Why a [`ForceRemoveOrder`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceRemoveOrderError {
    /// ADR 0037 requires an *explicit operator reason*; a blank reason is refused
    /// so the audit trail can never record an unexplained forced removal.
    EmptyReason,
}

impl std::fmt::Display for ForceRemoveOrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyReason => write!(f, "a forced removal requires an explicit operator reason"),
        }
    }
}

impl std::error::Error for ForceRemoveOrderError {}

/// A fully authorized order to force-remove a dead/unrecoverable member: the
/// administrative capability, the target member, and the explicit operator
/// reason. Built with [`ForceRemoveOrder::new`], which enforces the non-empty
/// reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRemoveOrder {
    capability: ForceCapability,
    member: NodeIdentity,
    reason: String,
}

impl ForceRemoveOrder {
    pub fn new(
        capability: ForceCapability,
        member: NodeIdentity,
        reason: impl Into<String>,
    ) -> Result<Self, ForceRemoveOrderError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ForceRemoveOrderError::EmptyReason);
        }
        Ok(Self {
            capability,
            member,
            reason,
        })
    }

    pub fn member(&self) -> &NodeIdentity {
        &self.member
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn capability(&self) -> &ForceCapability {
        &self.capability
    }
}

/// A forced promotion of one owned range away from the dead member to the
/// best-available surviving replica. Unlike a planned hand-off this proceeds even
/// when the target cannot prove it covers the commit watermark — `covers_watermark`
/// records whether it could, so the audit trail captures any possible
/// committed-write loss. `next` is the fenced catalog entry (epoch bumped) that
/// activation installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedPromotion {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub dead_owner: NodeIdentity,
    pub new_owner: NodeIdentity,
    /// Whether the promoted replica's applied log covers the commit watermark. When
    /// `false`, writes past the replica's applied point may be lost — the price of
    /// recovering a range whose owner is gone.
    pub covers_watermark: bool,
    /// The promoted replica's catch-up evidence, if any was known.
    pub evidence: Option<CatchUpEvidence>,
    pub next: RangeOwnership,
}

/// An owned range that could **not** be force-recovered: the dead member was its
/// owner and no surviving replica exists to promote. The range's data is lost
/// with the member — recorded so the operator sees exactly what was unrecoverable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedBlock {
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub dead_owner: NodeIdentity,
}

/// The plan for a forced removal: the owned ranges to force-promote, the
/// replicated ranges whose dead copy is dropped, and the owned ranges that are
/// unrecoverable. Produced by [`plan_force_remove`] without mutating any catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRemovePlan {
    pub member: NodeIdentity,
    pub reason: String,
    pub capability_holder: String,
    pub promotions: Vec<ForcedPromotion>,
    /// Ranges where the dead member was only a replica: its copy is dropped from
    /// the live owner's replica set (no epoch bump). Each entry is the catalog
    /// update to apply.
    pub replica_drops: Vec<RangeOwnership>,
    pub unrecoverable: Vec<ForcedBlock>,
}

/// Plan the forced removal of a dead/unrecoverable member **without** mutating any
/// catalog, under the ADR 0037 forced-ownership rules. For each owned range, pick
/// the best surviving replica (preferring one that covers the commit watermark,
/// then the furthest-applied) and force-promote it; a range with no surviving
/// replica is recorded as unrecoverable. For each replicated range, drop the dead
/// member's copy.
pub fn plan_force_remove(
    order: &ForceRemoveOrder,
    membership: &MembershipCatalog,
    ownership: &ShardOwnershipCatalog,
    signals: &impl ClusterSignals,
) -> ForceRemovePlan {
    let member = order.member();
    let mut promotions = Vec::new();
    let mut replica_drops = Vec::new();
    let mut unrecoverable = Vec::new();

    for range in ownership.entries() {
        let collection = range.collection().clone();
        let range_id = range.range_id();

        if range.owner() == member {
            let watermark = signals.commit_watermark(&collection, range_id);
            match select_force_target(range, member, membership, watermark, signals) {
                Some((target, covers_watermark, evidence)) => {
                    let next =
                        range.transfer_to(target.clone(), without(range.replicas(), &target));
                    promotions.push(ForcedPromotion {
                        collection,
                        range_id,
                        dead_owner: member.clone(),
                        new_owner: target,
                        covers_watermark,
                        evidence,
                        next,
                    });
                }
                None => unrecoverable.push(ForcedBlock {
                    collection,
                    range_id,
                    dead_owner: member.clone(),
                }),
            }
        } else if range.replicas().contains(member) {
            replica_drops.push(range.update_replicas(without(range.replicas(), member)));
        }
    }

    ForceRemovePlan {
        member: member.clone(),
        reason: order.reason().to_string(),
        capability_holder: order.capability().holder().to_string(),
        promotions,
        replica_drops,
        unrecoverable,
    }
}

/// The durable audit evidence of a forced removal: who authorized it, why, which
/// ranges moved (and whether each may have lost writes), which were unrecoverable,
/// and how many stale replica copies were dropped. ADR 0037 requires a forced
/// transition to leave exactly this trail; its [`Display`](std::fmt::Display) is a
/// single audit line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRemoveAudit {
    pub member: NodeIdentity,
    pub capability_holder: String,
    pub reason: String,
    /// `(collection, range_id, new_owner, covers_watermark)` for each forced
    /// promotion. A `false` flag marks a range that may have lost committed writes.
    pub promotions: Vec<(CollectionId, RangeId, NodeIdentity, bool)>,
    pub unrecoverable: Vec<(CollectionId, RangeId)>,
    pub replica_copies_dropped: usize,
}

impl ForceRemoveAudit {
    /// Whether any forced promotion could not prove it covered the commit
    /// watermark — i.e. the forced removal may have lost committed writes.
    pub fn has_potential_write_loss(&self) -> bool {
        self.promotions.iter().any(|(_, _, _, covers)| !covers) || !self.unrecoverable.is_empty()
    }
}

impl std::fmt::Display for ForceRemoveAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FORCE remove {} by {} (reason: {}): {} range(s) force-promoted, {} unrecoverable, {} stale replica copies dropped",
            self.member,
            self.capability_holder,
            self.reason,
            self.promotions.len(),
            self.unrecoverable.len(),
            self.replica_copies_dropped,
        )?;
        if self.has_potential_write_loss() {
            write!(f, "; POTENTIAL WRITE LOSS")?;
        }
        Ok(())
    }
}

/// The result of running a forced removal: the audit evidence, the activated
/// promotion outcomes, the unrecoverable ranges, and the removed member (if it was
/// a member). The dead member is removed from the catalog regardless of
/// unrecoverable ranges — it is gone; the unrecoverable ranges are recorded for
/// the operator, not a reason to keep a dead member listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRemoveResult {
    pub audit: ForceRemoveAudit,
    pub promotions: Vec<TransitionOutcome>,
    pub unrecoverable: Vec<ForcedBlock>,
    pub removed: Option<ClusterMember>,
}

/// Plan and execute a forced removal: force-promote each owned range's surviving
/// replica (fencing the dead owner via the epoch bump), drop the dead member's
/// stale replica copies, then remove it from membership. Returns the audit
/// evidence and outcomes. Unlike a planned drain, this never refuses — a dead
/// member is removed even when some of its ranges are unrecoverable.
pub fn run_force_remove(
    order: &ForceRemoveOrder,
    membership: &mut MembershipCatalog,
    ownership: &mut ShardOwnershipCatalog,
    signals: &impl ClusterSignals,
) -> ForceRemoveResult {
    let plan = plan_force_remove(order, membership, ownership, signals);

    let mut promotion_outcomes = Vec::new();
    let mut audit_promotions = Vec::new();
    for promotion in &plan.promotions {
        let previous_owner = promotion.dead_owner.clone();
        let new_epoch = promotion.next.epoch();
        let previous_epoch = ownership
            .range(&promotion.collection, promotion.range_id)
            .map(RangeOwnership::epoch)
            .unwrap_or(new_epoch);
        let new_version = promotion.next.version();
        let previous_version = ownership
            .range(&promotion.collection, promotion.range_id)
            .map(RangeOwnership::version)
            .unwrap_or(new_version);
        let watermark = signals.commit_watermark(&promotion.collection, promotion.range_id);
        if ownership.apply_update(promotion.next.clone()).is_ok() {
            audit_promotions.push((
                promotion.collection.clone(),
                promotion.range_id,
                promotion.new_owner.clone(),
                promotion.covers_watermark,
            ));
            promotion_outcomes.push(TransitionOutcome {
                kind: TransitionKind::Promote,
                collection: promotion.collection.clone(),
                range_id: promotion.range_id,
                previous_owner,
                new_owner: promotion.new_owner.clone(),
                previous_epoch,
                new_epoch,
                previous_version,
                new_version,
                watermark,
            });
        }
    }

    let mut replica_copies_dropped = 0;
    for drop in &plan.replica_drops {
        if ownership.apply_update(drop.clone()).is_ok() {
            replica_copies_dropped += 1;
        }
    }

    let removed = membership.remove(order.member());

    let audit = ForceRemoveAudit {
        member: order.member().clone(),
        capability_holder: plan.capability_holder,
        reason: plan.reason,
        promotions: audit_promotions,
        unrecoverable: plan
            .unrecoverable
            .iter()
            .map(|b| (b.collection.clone(), b.range_id))
            .collect(),
        replica_copies_dropped,
    };

    ForceRemoveResult {
        audit,
        promotions: promotion_outcomes,
        unrecoverable: plan.unrecoverable,
        removed,
    }
}

// =============================================================================
// Status reporting
// =============================================================================

/// A snapshot of one member's drain posture for operator/status reporting: its
/// draining flag, the ranges it still owns and replicates, the count of steps a
/// drain would schedule, the ranges currently blocking it, and whether it is
/// removable right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStatus {
    pub member: NodeIdentity,
    pub is_member: bool,
    pub is_draining: bool,
    pub owned_ranges: Vec<(CollectionId, RangeId)>,
    pub replicated_ranges: Vec<(CollectionId, RangeId)>,
    pub planned_steps: usize,
    pub blocked: Vec<DrainBlock>,
    /// True when the member is draining and depends on no range — a
    /// [`commit_drain_removal`] would succeed.
    pub removable: bool,
}

/// Report a member's current drain status without mutating anything — the read
/// side of the drain flow an operator surface renders.
pub fn drain_status(
    member: &NodeIdentity,
    membership: &MembershipCatalog,
    ownership: &ShardOwnershipCatalog,
    signals: &impl ClusterSignals,
) -> DrainStatus {
    let member_entry = membership.member(member);
    let is_member = member_entry.is_some();
    let is_draining = member_entry.is_some_and(ClusterMember::is_draining);
    let (owned_ranges, replicated_ranges) = range_dependencies(member, ownership);
    let plan = plan_drain(member, membership, ownership, signals);
    let removable = is_draining && owned_ranges.is_empty() && replicated_ranges.is_empty();

    DrainStatus {
        member: member.clone(),
        is_member,
        is_draining,
        owned_ranges,
        replicated_ranges,
        planned_steps: plan.steps.len(),
        blocked: plan.blocked,
        removable,
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

/// `(owned, replicated)` ranges for `member`, each in `(collection, range_id)`
/// order — the dependencies that must be cleared before removal.
fn range_dependencies(
    member: &NodeIdentity,
    ownership: &ShardOwnershipCatalog,
) -> (Vec<(CollectionId, RangeId)>, Vec<(CollectionId, RangeId)>) {
    let mut owned = Vec::new();
    let mut replicated = Vec::new();
    for range in ownership.entries() {
        if range.owner() == member {
            owned.push((range.collection().clone(), range.range_id()));
        } else if range.replicas().contains(member) {
            replicated.push((range.collection().clone(), range.range_id()));
        }
    }
    (owned, replicated)
}

/// `replicas` without `node`, preserving order.
fn without(replicas: &[NodeIdentity], node: &NodeIdentity) -> Vec<NodeIdentity> {
    replicas.iter().filter(|r| *r != node).cloned().collect()
}

/// The safest caught-up hand-off target for an owned range: a current replica that
/// is an active data member (so not the draining member, not a witness, not
/// another draining member) and whose catch-up evidence covers the commit
/// watermark. Prefers the furthest-applied candidate, breaking ties by stable
/// identity order. `None` when no replica is a safe target.
fn select_handoff_target(
    range: &RangeOwnership,
    member: &NodeIdentity,
    membership: &MembershipCatalog,
    watermark: CommitWatermark,
    signals: &impl ClusterSignals,
) -> Option<(NodeIdentity, CatchUpEvidence)> {
    let mut best: Option<(CatchUpEvidence, NodeIdentity)> = None;
    for candidate in range.replicas() {
        if candidate == member {
            continue;
        }
        if !membership
            .member(candidate)
            .is_some_and(ClusterMember::is_placement_eligible)
        {
            continue;
        }
        let Some(evidence) = signals.catch_up(range.collection(), range.range_id(), candidate)
        else {
            continue;
        };
        if !evidence.covers(watermark) {
            continue;
        }
        let applied = (evidence.applied_term, evidence.applied_lsn);
        let better = match &best {
            None => true,
            Some((best_ev, best_id)) => {
                applied > (best_ev.applied_term, best_ev.applied_lsn)
                    || (applied == (best_ev.applied_term, best_ev.applied_lsn)
                        && candidate < best_id)
            }
        };
        if better {
            best = Some((evidence, candidate.clone()));
        }
    }
    best.map(|(evidence, id)| (id, evidence))
}

/// The best surviving replica to force-promote for a dead owner's range, under the
/// forced-ownership rules: any current replica that is still an active data member
/// (other than the dead member). Prefers a replica that covers the commit
/// watermark, then the furthest-applied, then stable identity order — so the
/// forced promotion minimises loss even though it does not *require* coverage.
/// Returns `(target, covers_watermark, evidence)`, or `None` when no replica
/// survives.
fn select_force_target(
    range: &RangeOwnership,
    member: &NodeIdentity,
    membership: &MembershipCatalog,
    watermark: CommitWatermark,
    signals: &impl ClusterSignals,
) -> Option<(NodeIdentity, bool, Option<CatchUpEvidence>)> {
    // Rank key: (covers_watermark, (applied_term, applied_lsn)). A replica with no
    // evidence ranks at (false, (0, 0)) — still eligible (the owner is dead), just
    // least preferred.
    let mut best: Option<(bool, (u64, u64), NodeIdentity, Option<CatchUpEvidence>)> = None;
    for candidate in range.replicas() {
        if candidate == member {
            continue;
        }
        if !membership
            .member(candidate)
            .is_some_and(ClusterMember::is_placement_eligible)
        {
            continue;
        }
        let evidence = signals.catch_up(range.collection(), range.range_id(), candidate);
        let covers = evidence.as_ref().is_some_and(|e| e.covers(watermark));
        let applied = evidence
            .as_ref()
            .map(|e| (e.applied_term, e.applied_lsn))
            .unwrap_or((0, 0));
        let better = match &best {
            None => true,
            Some((best_covers, best_applied, best_id, _)) => {
                (covers, applied) > (*best_covers, *best_applied)
                    || ((covers, applied) == (*best_covers, *best_applied) && candidate < best_id)
            }
        };
        if better {
            best = Some((covers, applied, candidate.clone(), evidence));
        }
    }
    best.map(|(covers, _, id, evidence)| (id, covers, evidence))
}

/// An eligible host for a replacement replica copy when evacuating the draining
/// member would otherwise drop a range below its replication factor: an active
/// data member that is not the draining member and does not already hold the range
/// (as owner or replica). Lowest stable identity wins, for determinism. `None`
/// when no member is free to take a copy.
fn select_replacement_replica(
    range: &RangeOwnership,
    member: &NodeIdentity,
    membership: &MembershipCatalog,
) -> Option<NodeIdentity> {
    membership
        .placement_eligible_members()
        .map(ClusterMember::identity)
        .find(|id| *id != member && range.owner() != *id && !range.replicas().contains(id))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::membership::{ClusterId, MemberKind};
    use crate::cluster::ownership::{
        OwnershipEpoch, PlacementMetadata, RangeBounds, RangeRole, RangeWriteReject, ShardKeyMode,
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

    /// A single full-keyspace range `orders/1` owned by `owner` with `replicas` and
    /// the given replication factor.
    fn catalog_with_rf(
        owner: &str,
        replicas: &[&str],
        rf: usize,
    ) -> (ShardOwnershipCatalog, CollectionId) {
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
                PlacementMetadata::with_replication_factor(rf),
            ))
            .unwrap();
        (catalog, orders)
    }

    fn catalog_with(owner: &str, replicas: &[&str]) -> (ShardOwnershipCatalog, CollectionId) {
        catalog_with_rf(owner, replicas, 3)
    }

    /// A scripted [`ClusterSignals`]: one shared watermark and per-candidate
    /// catch-up evidence keyed by CN. Health signals are not consulted by the drain
    /// flows, so only the watermark/catch-up surface is scripted.
    struct FakeSignals {
        watermark: CommitWatermark,
        catch_up: HashMap<NodeIdentity, CatchUpEvidence>,
    }

    impl FakeSignals {
        fn new(watermark: CommitWatermark) -> Self {
            Self {
                watermark,
                catch_up: HashMap::new(),
            }
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
        fn member_signals(
            &self,
            _member: &NodeIdentity,
        ) -> crate::cluster::supervisor::MemberSignals {
            crate::cluster::supervisor::MemberSignals::healthy()
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

    // --- membership drain state -------------------------------------------

    #[test]
    fn begin_drain_marks_member_and_excludes_from_placement() {
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        assert!(members
            .member(&ident("CN=node-a"))
            .unwrap()
            .is_placement_eligible());

        let changed = members.begin_drain(&ident("CN=node-a"));
        assert_eq!(changed, Some(true));
        assert!(members.member(&ident("CN=node-a")).unwrap().is_draining());
        // Idempotent: marking again reports no change.
        assert_eq!(members.begin_drain(&ident("CN=node-a")), Some(false));
        // A draining member is no longer a placement target.
        assert!(!members
            .member(&ident("CN=node-a"))
            .unwrap()
            .is_placement_eligible());
        let eligible: Vec<_> = members
            .placement_eligible_members()
            .map(|m| m.identity().clone())
            .collect();
        assert_eq!(eligible, vec![ident("CN=node-b")]);
        // A non-member cannot be drained.
        assert_eq!(members.begin_drain(&ident("CN=ghost")), None);
    }

    // --- successful drain --------------------------------------------------

    #[test]
    fn successful_drain_moves_all_ranges_then_allows_removal() {
        // node-a owns orders/1 and replicates a second range; both must move before
        // it can be removed.
        let mut members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        // orders/1 owned by node-a, replicated by node-b and node-c.
        catalog
            .apply_update(RangeOwnership::establish(
                orders.clone(),
                RangeId::new(1),
                ShardKeyMode::Hash,
                RangeBounds::full(),
                ident("CN=node-a"),
                vec![ident("CN=node-b"), ident("CN=node-c")],
                PlacementMetadata::with_replication_factor(2),
            ))
            .unwrap();
        // events/1 owned by node-b, replicated by node-a (over-replicated: rf 1).
        let events = collection("events");
        catalog
            .apply_update(RangeOwnership::establish(
                events.clone(),
                RangeId::new(1),
                ShardKeyMode::Hash,
                RangeBounds::full(),
                ident("CN=node-b"),
                vec![ident("CN=node-a")],
                PlacementMetadata::with_replication_factor(1),
            ))
            .unwrap();

        members.begin_drain(&ident("CN=node-a")).unwrap();
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        let outcome = run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        assert!(outcome.is_drained(), "every range moved off node-a");
        assert_eq!(outcome.handoffs.len(), 1);
        assert!(outcome.handoffs[0].is_ok());
        assert_eq!(outcome.evacuations.len(), 1);

        // orders/1 is now owned by a caught-up replica (node-b, identity tie-break),
        // and node-a is fenced from public writes.
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-b"));
        assert!(r1.epoch().value() > 1, "epoch bumped to fence old owner");
        // events/1 no longer lists node-a as a replica.
        let r2 = catalog.range(&events, RangeId::new(1)).unwrap();
        assert!(!r2.replicas().contains(&ident("CN=node-a")));

        // node-a now depends on no range: removal is allowed.
        let removed = commit_drain_removal(&ident("CN=node-a"), &mut members, &catalog)
            .expect("drained member is removable");
        assert_eq!(removed.identity(), &ident("CN=node-a"));
        assert!(!members.is_authorized(&ident("CN=node-a")));

        // The fenced old owner is rejected if it still tries to write orders/1.
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

    // --- drain blocked by an unmoved range --------------------------------

    #[test]
    fn drain_blocked_by_unmoved_range_refuses_removal() {
        // node-a owns orders/1 whose only replica node-b has NOT caught up to the
        // watermark, so there is no safe hand-off target. The range stays, and
        // removal is refused.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        members.begin_drain(&ident("CN=node-a")).unwrap();
        let signals =
            FakeSignals::new(CommitWatermark::new(2, 50)).with_catch_up("CN=node-b", 2, 49); // one LSN short

        let outcome = run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        assert!(!outcome.is_drained());
        assert!(outcome.handoffs.is_empty());
        assert_eq!(outcome.blocked.len(), 1);
        assert_eq!(
            outcome.blocked[0].reason,
            DrainBlockReason::NoSafeHandoffTarget
        );

        // Ownership is untouched — node-a still owns orders/1.
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-a"));
        assert_eq!(r1.epoch(), OwnershipEpoch::initial());

        // Removal is refused while the range still depends on node-a.
        let err = commit_drain_removal(&ident("CN=node-a"), &mut members, &catalog).unwrap_err();
        match err {
            RemovalRejection::StillOwnsRanges { ranges, .. } => {
                assert_eq!(ranges, vec![(orders.clone(), RangeId::new(1))]);
            }
            other => panic!("expected StillOwnsRanges, got {other:?}"),
        }
        assert!(members.is_authorized(&ident("CN=node-a")), "still a member");
    }

    #[test]
    fn drain_blocked_when_replica_evac_would_drop_below_rf() {
        // node-a replicates orders/1 (owner node-b) at exactly rf 2, and there is no
        // free member to host a replacement copy — evacuation is blocked.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, _orders) = catalog_with_rf("CN=node-b", &["CN=node-a"], 2);
        members.begin_drain(&ident("CN=node-a")).unwrap();
        let signals = FakeSignals::new(CommitWatermark::new(1, 10));

        let outcome = run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        assert_eq!(outcome.blocked.len(), 1);
        assert_eq!(
            outcome.blocked[0].reason,
            DrainBlockReason::NoReplacementReplica
        );
    }

    #[test]
    fn replica_evac_assigns_replacement_to_preserve_rf() {
        // node-a replicates orders/1 (owner node-b) at rf 2; node-c is free to take
        // a replacement copy, so the evacuation moves the copy rather than blocking.
        let mut members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (mut catalog, orders) = catalog_with_rf("CN=node-b", &["CN=node-a"], 2);
        members.begin_drain(&ident("CN=node-a")).unwrap();
        let signals = FakeSignals::new(CommitWatermark::new(1, 10));

        let plan = plan_drain(&ident("CN=node-a"), &members, &catalog, &signals);
        assert_eq!(plan.steps.len(), 1);
        match &plan.steps[0] {
            DrainStep::Evacuate(evac) => {
                assert_eq!(evac.replacement, Some(ident("CN=node-c")));
            }
            other => panic!("expected Evacuate, got {other:?}"),
        }

        run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert!(!r1.replicas().contains(&ident("CN=node-a")));
        assert!(r1.replicas().contains(&ident("CN=node-c")));
        // Owner unchanged and epoch not bumped — only the replica set moved.
        assert_eq!(r1.owner(), &ident("CN=node-b"));
        assert_eq!(r1.epoch(), OwnershipEpoch::initial());
    }

    // --- no new placements to a draining member ---------------------------

    #[test]
    fn draining_member_is_never_a_handoff_or_replacement_target() {
        // node-a (draining) owns orders/1 with replicas node-b (also draining) and
        // node-c. The hand-off must skip the draining node-b and choose node-c.
        let mut members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        members.begin_drain(&ident("CN=node-a")).unwrap();
        members.begin_drain(&ident("CN=node-b")).unwrap();
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        let plan = plan_drain(&ident("CN=node-a"), &members, &catalog, &signals);
        match &plan.steps[0] {
            DrainStep::Handoff(h) => assert_eq!(
                h.target,
                ident("CN=node-c"),
                "draining node-b is not a placement target"
            ),
            other => panic!("expected Handoff, got {other:?}"),
        }

        run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-c"));
    }

    // --- force remove recovery --------------------------------------------

    #[test]
    fn force_remove_promotes_surviving_replica_and_fences_dead_owner() {
        // node-a is dead; it owned orders/1 with a caught-up replica node-b.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals =
            FakeSignals::new(CommitWatermark::new(1, 10)).with_catch_up("CN=node-b", 1, 10);
        let order = ForceRemoveOrder::new(
            ForceCapability::granted("ops:alice"),
            ident("CN=node-a"),
            "node-a hardware failure, unrecoverable",
        )
        .unwrap();

        let result = run_force_remove(&order, &mut members, &mut catalog, &signals);
        assert_eq!(result.promotions.len(), 1);
        assert_eq!(result.promotions[0].new_owner, ident("CN=node-b"));
        assert!(result.promotions[0].fenced_old_owner());
        assert!(result.unrecoverable.is_empty());
        // The dead member is removed.
        assert!(result.removed.is_some());
        assert!(!members.is_authorized(&ident("CN=node-a")));

        // node-b owns orders/1 at a bumped epoch; node-a is fenced.
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-b"));
        assert_eq!(r1.role_of(&ident("CN=node-b")), RangeRole::Owner);
        assert!(r1.epoch().value() > 1);

        // The audit covers the watermark — no write loss here.
        assert!(!result.audit.has_potential_write_loss());
        let line = result.audit.to_string();
        assert!(line.contains("FORCE remove"));
        assert!(line.contains("ops:alice"));
        assert!(line.contains("hardware failure"));
    }

    #[test]
    fn force_remove_proceeds_with_behind_replica_and_records_write_loss() {
        // node-a is dead; its only replica node-b is BEHIND the watermark. Ordinary
        // failover would block, but a forced removal promotes it anyway and records
        // the potential committed-write loss as audit evidence.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let signals =
            FakeSignals::new(CommitWatermark::new(2, 50)).with_catch_up("CN=node-b", 2, 49); // one LSN short
        let order = ForceRemoveOrder::new(
            ForceCapability::granted("ops:bob"),
            ident("CN=node-a"),
            "node-a disk destroyed",
        )
        .unwrap();

        let result = run_force_remove(&order, &mut members, &mut catalog, &signals);
        assert_eq!(result.promotions.len(), 1);
        assert!(!result.audit.promotions[0].3, "does not cover watermark");
        assert!(result.audit.has_potential_write_loss());
        assert!(result.audit.to_string().contains("POTENTIAL WRITE LOSS"));

        // It still moved authority and fenced the dead owner.
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-b"));
    }

    #[test]
    fn force_remove_records_unrecoverable_range_with_no_replica() {
        // node-a is dead and owned orders/1 with NO replica — the range is
        // unrecoverable, recorded in the audit, but node-a is still removed.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-a", &[]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10));
        let order = ForceRemoveOrder::new(
            ForceCapability::granted("ops:carol"),
            ident("CN=node-a"),
            "node-a lost, no replicas existed",
        )
        .unwrap();

        let result = run_force_remove(&order, &mut members, &mut catalog, &signals);
        assert!(result.promotions.is_empty());
        assert_eq!(result.unrecoverable.len(), 1);
        assert_eq!(result.unrecoverable[0].range_id, RangeId::new(1));
        assert!(result.audit.has_potential_write_loss());
        assert!(result.removed.is_some());
        assert!(!members.is_authorized(&ident("CN=node-a")));

        // The orphaned range still names node-a (it is unrecoverable, not silently
        // reassigned).
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-a"));
    }

    #[test]
    fn force_remove_drops_dead_members_stale_replica_copies() {
        // node-a is dead and was only a replica of orders/1 (owner node-b). Force
        // remove drops its stale copy and removes it; the live owner is untouched.
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        let (mut catalog, orders) = catalog_with("CN=node-b", &["CN=node-a"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10));
        let order = ForceRemoveOrder::new(
            ForceCapability::granted("ops:dan"),
            ident("CN=node-a"),
            "node-a gone",
        )
        .unwrap();

        let result = run_force_remove(&order, &mut members, &mut catalog, &signals);
        assert!(result.promotions.is_empty());
        assert_eq!(result.audit.replica_copies_dropped, 1);
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(r1.owner(), &ident("CN=node-b"));
        assert!(!r1.replicas().contains(&ident("CN=node-a")));
        assert_eq!(r1.epoch(), OwnershipEpoch::initial(), "owner unchanged");
    }

    #[test]
    fn force_remove_order_requires_explicit_reason() {
        let err = ForceRemoveOrder::new(
            ForceCapability::granted("ops:eve"),
            ident("CN=node-a"),
            "   ",
        )
        .unwrap_err();
        assert_eq!(err, ForceRemoveOrderError::EmptyReason);
    }

    // --- audit / status reporting -----------------------------------------

    #[test]
    fn drain_status_reports_dependencies_and_removability() {
        let mut members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (mut catalog, _orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::new(CommitWatermark::new(1, 10))
            .with_catch_up("CN=node-b", 1, 10)
            .with_catch_up("CN=node-c", 1, 10);

        // Before draining: a member, owns one range, not draining, not removable.
        let status = drain_status(&ident("CN=node-a"), &members, &catalog, &signals);
        assert!(status.is_member);
        assert!(!status.is_draining);
        assert_eq!(status.owned_ranges.len(), 1);
        assert!(status.replicated_ranges.is_empty());
        assert_eq!(status.planned_steps, 1);
        assert!(!status.removable);

        // After marking draining and running the drain: no dependencies, removable.
        members.begin_drain(&ident("CN=node-a")).unwrap();
        run_drain(&ident("CN=node-a"), &members, &mut catalog, &signals);
        let status = drain_status(&ident("CN=node-a"), &members, &catalog, &signals);
        assert!(status.is_draining);
        assert!(status.owned_ranges.is_empty());
        assert!(status.replicated_ranges.is_empty());
        assert!(status.removable);
    }

    #[test]
    fn removing_a_non_member_or_non_draining_member_is_refused() {
        let mut members = membership(&["CN=node-a"]);
        let catalog = ShardOwnershipCatalog::new();

        // Not a member.
        let err = commit_drain_removal(&ident("CN=ghost"), &mut members, &catalog).unwrap_err();
        assert!(matches!(err, RemovalRejection::NotAMember { .. }));

        // A member that was never marked draining cannot be removed via the planned
        // path (force-remove is the path for that).
        let err = commit_drain_removal(&ident("CN=node-a"), &mut members, &catalog).unwrap_err();
        assert!(matches!(err, RemovalRejection::NotDraining { .. }));
    }
}
