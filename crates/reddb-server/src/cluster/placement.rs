//! Weighted placement and the multi-signal rebalancer planner (issue #1003,
//! PRD #987, ADR 0037).
//!
//! Where the [`supervisor`](super::supervisor) reacts to a *failed* owner, this
//! module is the proactive counterpart: it decides where ranges *should* live so
//! the cluster's storage and traffic stay balanced as members come, go, and grow
//! their disks. It is the glossary's **weighted placement** policy
//! (`clustering.md`) — *"shard/range placement policy that accounts for advertised
//! node capacity such as usable disk … and operator weights. Expanding a node's
//! disk changes its placement weight; data moves only through explicit rebalancing
//! transitions"* — driven by the **multi-signal rebalancer** — *"Cluster
//! Supervisor policy that plans ownership transitions using bytes-used versus
//! weighted capacity as the primary safety signal and read/write load as a
//! secondary hotspot signal."*
//!
//! ## Two signals, one a safety floor and one a hint
//!
//! * **Primary — bytes-used vs weighted capacity.** Every member advertises its
//!   usable disk and an operator weight ([`MemberCapacity`]); the product is its
//!   [`weighted_capacity`](MemberCapacity::weighted_capacity), the member's share
//!   of the cluster it is *meant* to hold. The planner compares each member's
//!   bytes-used against its **fair share** (cluster bytes apportioned by weighted
//!   capacity) and proposes moving ranges off members that are over their share
//!   onto members that are under it. This is the safety signal: a member running
//!   out of disk is an availability risk, so capacity balance is what the planner
//!   acts on.
//! * **Secondary — read/write load.** A range can be perfectly placed by bytes
//!   yet still be a **hotspot**: it absorbs a disproportionate share of the
//!   cluster's read/write traffic. The planner surfaces hotspots
//!   ([`HotspotRange`]) and, when capacity allows, proposes spreading them off
//!   their over-loaded owner. This is a hint layered on top of the capacity
//!   floor, never in place of it — a hotspot move is only taken when it does not
//!   itself create a capacity problem.
//!
//! ## Planning, not moving
//!
//! [`WeightedPlacementPlanner::plan_rebalance`] reads the membership catalog, the
//! ownership catalog, and the live signals, and returns a [`RebalancePlan`] of
//! [`PlannedMove`]s. It takes the ownership catalog by shared reference and
//! **never mutates it** — *nothing moves implicitly*. Each [`PlannedMove`] is the
//! intent for one rebalancing transition; executing it (copy the range to the
//! target, let it catch up to the range commit watermark, then cut over through
//! the fenced [`Handoff`](super::ownership_transition::TransitionKind::Handoff)
//! transition machine) is a separate, explicit step. This is why *expanding a
//! member's disk changes its placement weight but moves no data*: the new weight
//! changes what the *next* plan proposes, and data only relocates when that plan
//! is run.
//!
//! ## Purity
//!
//! All live state — per-member advertised capacity and per-range bytes/traffic —
//! is read through the [`PlacementSignals`] trait, injected by the caller.
//! Production backs it onto the disk-usage reporter and the per-range traffic
//! counters; tests back it onto a scripted fake. The planner itself is a pure
//! policy over the two catalogs plus those signals, so the whole weighting,
//! balancing, and hotspot story is exercised deterministically — no disk, no
//! clock, no network.

use std::collections::{BTreeMap, BTreeSet};

use super::identity::NodeIdentity;
use super::membership::MembershipCatalog;
use super::ownership::{CollectionId, RangeId, ShardOwnershipCatalog};

/// The neutral operator weight: a member with this weight is placed strictly by
/// its usable disk. The weight is expressed in hundredths, so `100` means a 1.0×
/// multiplier; `200` doubles a member's placement weight and `50` halves it. An
/// operator nudges placement without lying about disk by tuning this.
pub const NEUTRAL_OPERATOR_WEIGHT: u32 = 100;

/// Authority-sharding unit for placement. Related small collections may share a
/// group; a large collection can be isolated in its own group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CollectionGroupId(String);

impl CollectionGroupId {
    pub fn new(value: impl Into<String>) -> Result<Self, PlacementAuthorityError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PlacementAuthorityError::EmptyCollectionGroup);
        }
        Ok(Self(value))
    }
}

impl std::fmt::Display for CollectionGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The Placement Authority responsible for one Collection group and the
/// collections whose ownership-catalog slice belongs to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionGroupPlacementAuthority {
    collection_group: CollectionGroupId,
    authority: NodeIdentity,
    collections: BTreeSet<CollectionId>,
}

impl CollectionGroupPlacementAuthority {
    pub fn new(
        collection_group: CollectionGroupId,
        authority: NodeIdentity,
        collections: impl IntoIterator<Item = CollectionId>,
    ) -> Result<Self, PlacementAuthorityError> {
        let collections: BTreeSet<_> = collections.into_iter().collect();
        if collections.is_empty() {
            return Err(PlacementAuthorityError::EmptyCollectionSet {
                collection_group,
                authority,
            });
        }
        Ok(Self {
            collection_group,
            authority,
            collections,
        })
    }

    pub fn collection_group(&self) -> &CollectionGroupId {
        &self.collection_group
    }

    pub fn authority(&self) -> &NodeIdentity {
        &self.authority
    }

    pub fn covers(&self, collection: &CollectionId) -> bool {
        self.collections.contains(collection)
    }
}

/// Pure in-memory authority index used by placement planning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementAuthorityCatalog {
    by_collection: BTreeMap<CollectionId, CollectionGroupPlacementAuthority>,
}

impl PlacementAuthorityCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        authority: CollectionGroupPlacementAuthority,
    ) -> Result<(), PlacementAuthorityError> {
        for collection in &authority.collections {
            if let Some(existing) = self.by_collection.get(collection) {
                return Err(PlacementAuthorityError::OverlappingCollection {
                    collection: collection.clone(),
                    existing_group: existing.collection_group.clone(),
                    new_group: authority.collection_group.clone(),
                });
            }
        }
        for collection in &authority.collections {
            self.by_collection
                .insert(collection.clone(), authority.clone());
        }
        Ok(())
    }

    pub fn authority_for(
        &self,
        collection: &CollectionId,
    ) -> Option<&CollectionGroupPlacementAuthority> {
        self.by_collection.get(collection)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementAuthorityError {
    EmptyCollectionGroup,
    EmptyCollectionSet {
        collection_group: CollectionGroupId,
        authority: NodeIdentity,
    },
    OverlappingCollection {
        collection: CollectionId,
        existing_group: CollectionGroupId,
        new_group: CollectionGroupId,
    },
    MissingCollectionAuthority {
        collection: CollectionId,
    },
}

impl std::fmt::Display for PlacementAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCollectionGroup => write!(f, "collection group id must not be empty"),
            Self::EmptyCollectionSet {
                collection_group,
                authority,
            } => write!(
                f,
                "placement authority {authority} for collection group {collection_group} has no collections"
            ),
            Self::OverlappingCollection {
                collection,
                existing_group,
                new_group,
            } => write!(
                f,
                "collection {collection} is already assigned to collection group {existing_group}, cannot also assign it to {new_group}"
            ),
            Self::MissingCollectionAuthority { collection } => {
                write!(f, "no placement authority for collection {collection}")
            }
        }
    }
}

impl std::error::Error for PlacementAuthorityError {}

/// A member's advertised placement capacity: how much usable disk it offers and
/// the operator's weight multiplier on top of it.
///
/// The two combine into the member's [`weighted_capacity`](Self::weighted_capacity)
/// — its share of the cluster it is meant to hold. Advertising more usable disk,
/// or a higher operator weight, raises that share; the planner then apportions
/// ranges toward it on the *next* plan. The struct is pure advertised state: it
/// records what a member *offers*, never moves anything by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberCapacity {
    /// Usable disk the member advertises for user ranges, in bytes.
    pub usable_disk_bytes: u64,
    /// Operator weight in hundredths ([`NEUTRAL_OPERATOR_WEIGHT`] = 1.0×). Lets an
    /// operator bias placement on or off a member without misreporting disk.
    pub operator_weight: u32,
}

impl MemberCapacity {
    /// Capacity with an explicit usable disk and operator weight.
    pub fn new(usable_disk_bytes: u64, operator_weight: u32) -> Self {
        Self {
            usable_disk_bytes,
            operator_weight,
        }
    }

    /// Capacity from usable disk alone, at the neutral operator weight — the
    /// common case where the operator has expressed no preference.
    pub fn with_disk(usable_disk_bytes: u64) -> Self {
        Self::new(usable_disk_bytes, NEUTRAL_OPERATOR_WEIGHT)
    }

    /// The member's **placement weight**: usable disk scaled by the operator
    /// weight. This is the value the rebalancer apportions the cluster's bytes by,
    /// and it is exactly what *expanding a member's disk changes* — a larger disk
    /// (or a higher operator weight) yields a larger weighted capacity and so a
    /// larger fair share on the next plan. Computed in `u128` so a large disk
    /// times a large weight cannot overflow.
    pub fn weighted_capacity(&self) -> u128 {
        self.usable_disk_bytes as u128 * self.operator_weight as u128
            / NEUTRAL_OPERATOR_WEIGHT as u128
    }

    /// Whether this member can hold any ranges at all — a member advertising no
    /// usable disk (or a zero operator weight) has zero weighted capacity and is
    /// never a placement target.
    pub fn is_placeable(&self) -> bool {
        self.weighted_capacity() > 0
    }
}

/// The live load on one range: its on-disk size and its recent read/write
/// traffic.
///
/// `bytes_used` feeds the **primary** capacity signal (it is what a member's
/// bytes-used is summed from); `read_ops`/`write_ops` feed the **secondary**
/// hotspot signal. Keeping both on one struct lets a single
/// [`PlacementSignals::range_load`] call answer everything the planner needs about
/// a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RangeLoad {
    /// The range's on-disk size in bytes — the primary capacity signal.
    pub bytes_used: u64,
    /// Read operations served in the recent observation window.
    pub read_ops: u64,
    /// Write operations served in the recent observation window.
    pub write_ops: u64,
}

impl RangeLoad {
    /// A range that occupies `bytes_used` but serves no traffic — handy when only
    /// the capacity signal matters.
    pub fn idle(bytes_used: u64) -> Self {
        Self {
            bytes_used,
            read_ops: 0,
            write_ops: 0,
        }
    }

    /// Total read + write traffic — the hotspot signal. A range with high traffic
    /// relative to the cluster mean is a hotspot candidate regardless of its size.
    pub fn traffic(&self) -> u64 {
        self.read_ops.saturating_add(self.write_ops)
    }
}

/// The live cluster state the planner reads but does not own: each member's
/// advertised capacity and each range's bytes/traffic.
///
/// Production backs this onto the disk-usage reporter and the per-range traffic
/// counters; tests back it onto a scripted fake. Keeping it behind a trait is what
/// makes the planner a pure policy.
pub trait PlacementSignals {
    /// The capacity `member` currently advertises. A member that advertises
    /// nothing (or is unknown) should report a zero-disk [`MemberCapacity`], which
    /// makes it un-placeable rather than a div-by-zero hazard.
    fn member_capacity(&self, member: &NodeIdentity) -> MemberCapacity;

    /// The current load on `(collection, range_id)` — its bytes and its recent
    /// read/write traffic.
    fn range_load(&self, collection: &CollectionId, range_id: RangeId) -> RangeLoad;
}

/// Why the planner proposed moving a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveReason {
    /// The **primary** signal: the source member is over its weighted-capacity
    /// fair share and the target is under its own. Moving the range relieves a
    /// disk-pressure (availability) risk.
    CapacityBalance,
    /// The **secondary** signal: the range is a read/write hotspot on an
    /// over-loaded owner, and a target with both load and capacity headroom can
    /// absorb it. Taken only when it does not create a capacity problem.
    HotspotRelief,
}

/// One proposed rebalancing transition: move authority for a range from its
/// current owner to a target member.
///
/// A [`PlannedMove`] is *intent*, not an executed transition. Carrying it out
/// means copying the range to `to`, letting it catch up to the range commit
/// watermark, and then cutting over through the fenced
/// [`Handoff`](super::ownership_transition::TransitionKind::Handoff) machine — a
/// separate, explicit step. The planner only ever produces these; it moves no
/// data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMove {
    pub collection: CollectionId,
    pub range_id: RangeId,
    /// The range's current owner in the catalog — the move's source.
    pub from: NodeIdentity,
    /// The proposed new owner — an active data member with capacity headroom.
    pub to: NodeIdentity,
    /// The range's size in bytes at planning time (what the move relocates).
    pub bytes: u64,
    pub reason: MoveReason,
}

/// A range the **secondary** signal flagged as a read/write hotspot: it serves
/// traffic well above the cluster mean. Surfaced whether or not a relief move was
/// possible, so an operator can see a hotspot even when there is no headroom to
/// relieve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotRange {
    pub collection: CollectionId,
    pub range_id: RangeId,
    /// The range's current owner — the member bearing the hot traffic.
    pub owner: NodeIdentity,
    /// The range's read + write traffic in the observation window.
    pub traffic: u64,
}

/// The planner's decision for one pass: the moves to schedule and the hotspots it
/// observed.
///
/// A cluster already balanced by capacity, with no hotspot, yields an empty plan
/// ([`is_empty`](Self::is_empty)) — the no-op a stable cluster must produce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebalancePlan {
    /// Proposed moves, in a deterministic order (capacity moves first, then
    /// hotspot-relief moves, each in `(collection, range_id)` order).
    pub moves: Vec<PlannedMove>,
    /// Ranges observed to be hotspots this pass, hottest first.
    pub hotspots: Vec<HotspotRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityScopedPlannedMove {
    pub movement: PlannedMove,
    pub placement_authority: CollectionGroupPlacementAuthority,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorityScopedRebalancePlan {
    pub moves: Vec<AuthorityScopedPlannedMove>,
    pub hotspots: Vec<HotspotRange>,
}

impl RebalancePlan {
    /// Nothing to schedule *and* nothing hot — a fully balanced, evenly-loaded
    /// cluster. Distinct from [`no_moves`](Self::no_moves): a balanced cluster can
    /// still have an *observed* hotspot it cannot relieve.
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.hotspots.is_empty()
    }

    /// Whether the plan proposes any actual range movement. False on a cluster
    /// that is balanced by capacity and has no relievable hotspot, even if a
    /// hotspot was *observed*.
    pub fn no_moves(&self) -> bool {
        self.moves.is_empty()
    }

    /// The capacity-balance moves only (the primary signal).
    pub fn capacity_moves(&self) -> impl Iterator<Item = &PlannedMove> {
        self.moves
            .iter()
            .filter(|m| m.reason == MoveReason::CapacityBalance)
    }

    /// The hotspot-relief moves only (the secondary signal).
    pub fn hotspot_moves(&self) -> impl Iterator<Item = &PlannedMove> {
        self.moves
            .iter()
            .filter(|m| m.reason == MoveReason::HotspotRelief)
    }
}

/// The tunables that gate when imbalance and traffic are worth a move.
///
/// The defaults are deliberately slack: a cluster within 10% of its fair share is
/// "balanced enough" not to churn ownership, and a hotspot must run at 2× the
/// cluster-mean traffic before it is worth spreading. Tight thresholds would make
/// the planner thrash on noise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacementPolicy {
    /// Fractional tolerance around a member's fair share. A member is "over" only
    /// when its bytes-used exceeds `fair * (1 + balance_tolerance)`; a move's
    /// target must have room under `fair * (1 + balance_tolerance)`. Larger values
    /// tolerate more imbalance for less churn.
    pub balance_tolerance: f64,
    /// How many times the cluster-mean range traffic a range must serve to count
    /// as a hotspot. `2.0` means "twice the average".
    pub hotspot_load_factor: f64,
}

impl Default for PlacementPolicy {
    fn default() -> Self {
        Self {
            balance_tolerance: 0.10,
            hotspot_load_factor: 2.0,
        }
    }
}

/// The fair share of `total_bytes` that a member with `member_capacity` deserves,
/// out of the cluster's `total_capacity`. Apportions bytes strictly by weighted
/// capacity; `u128` math keeps a large cluster from overflowing.
fn fair_share(total_bytes: u64, member_capacity: u128, total_capacity: u128) -> u64 {
    if total_capacity == 0 {
        return 0;
    }
    let share = total_bytes as u128 * member_capacity / total_capacity;
    share.min(u64::MAX as u128) as u64
}

/// The weighted-placement, multi-signal rebalancer planner.
///
/// Holds only the [`PlacementPolicy`]; all live state is read through
/// [`PlacementSignals`] at plan time, so one planner instance serves the whole
/// cluster lifetime.
#[derive(Debug, Clone, Default)]
pub struct WeightedPlacementPlanner {
    policy: PlacementPolicy,
}

impl WeightedPlacementPlanner {
    /// A planner with the given policy.
    pub fn new(policy: PlacementPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &PlacementPolicy {
        &self.policy
    }

    /// Plan a rebalance across the whole ownership catalog **without** mutating
    /// it. Runs the primary capacity-balance pass, then the secondary
    /// hotspot-relief pass on top, and returns the combined [`RebalancePlan`].
    /// Ranges owned by members that are not placement-eligible (draining members,
    /// witnesses) are left to the drain flow and never moved here.
    pub fn plan_rebalance(
        &self,
        membership: &MembershipCatalog,
        ownership: &ShardOwnershipCatalog,
        signals: &impl PlacementSignals,
    ) -> RebalancePlan {
        let mut state = ClusterState::observe(membership, ownership, signals, &self.policy);
        let mut moves = state.plan_capacity_moves(&self.policy);
        let (hotspots, hotspot_moves) = state.plan_hotspot_moves(&self.policy);
        moves.extend(hotspot_moves);
        RebalancePlan { moves, hotspots }
    }

    pub fn plan_rebalance_scoped(
        &self,
        membership: &MembershipCatalog,
        ownership: &ShardOwnershipCatalog,
        signals: &impl PlacementSignals,
        authorities: &PlacementAuthorityCatalog,
    ) -> Result<AuthorityScopedRebalancePlan, PlacementAuthorityError> {
        let plan = self.plan_rebalance(membership, ownership, signals);
        let mut moves = Vec::with_capacity(plan.moves.len());
        for movement in plan.moves {
            let placement_authority = authorities
                .authority_for(&movement.collection)
                .ok_or_else(|| PlacementAuthorityError::MissingCollectionAuthority {
                    collection: movement.collection.clone(),
                })?
                .clone();
            moves.push(AuthorityScopedPlannedMove {
                movement,
                placement_authority,
            });
        }
        Ok(AuthorityScopedRebalancePlan {
            moves,
            hotspots: plan.hotspots,
        })
    }
}

/// The mutable simulation the planner balances against. Built once from the live
/// catalogs and signals, then evolved as moves are chosen so each successive move
/// sees the effect of the ones before it. Crucially this is a *copy* of the live
/// state — evolving it changes nothing in the real catalog, which is what makes
/// planning side-effect free.
struct ClusterState {
    /// Placement-eligible members (active data members) with non-zero weighted
    /// capacity, in stable identity order.
    eligible: Vec<NodeIdentity>,
    weighted_capacity: BTreeMap<NodeIdentity, u128>,
    /// Total weighted capacity across `eligible` — the denominator of fair share.
    total_capacity: u128,
    /// Total bytes across all movable ranges — the numerator of fair share.
    total_bytes: u64,
    /// Per-range size and traffic, keyed in `(collection, range_id)` order.
    ranges: BTreeMap<(CollectionId, RangeId), RangeFacts>,
    /// Simulated current owner of each movable range (evolves as moves are taken).
    owner_of: BTreeMap<(CollectionId, RangeId), NodeIdentity>,
    /// The range's true catalog owner — the `from` every move records, even if the
    /// simulation has since reassigned it (a range moves at most once per plan).
    origin_owner: BTreeMap<(CollectionId, RangeId), NodeIdentity>,
    /// Simulated bytes-used per member.
    used: BTreeMap<NodeIdentity, u64>,
    /// Simulated read/write load per member.
    load: BTreeMap<NodeIdentity, u64>,
    /// Ranges already scheduled to move — never moved twice in one plan.
    moved: std::collections::BTreeSet<(CollectionId, RangeId)>,
}

#[derive(Clone, Copy)]
struct RangeFacts {
    bytes: u64,
    traffic: u64,
}

impl ClusterState {
    fn observe(
        membership: &MembershipCatalog,
        ownership: &ShardOwnershipCatalog,
        signals: &impl PlacementSignals,
        _policy: &PlacementPolicy,
    ) -> Self {
        let mut weighted_capacity = BTreeMap::new();
        let mut eligible = Vec::new();
        let mut total_capacity: u128 = 0;
        for member in membership.placement_eligible_members() {
            let id = member.identity().clone();
            let cap = signals.member_capacity(&id).weighted_capacity();
            if cap == 0 {
                // A placement-eligible member advertising no usable disk is not a
                // valid target; exclude it so it is never apportioned bytes.
                continue;
            }
            total_capacity += cap;
            weighted_capacity.insert(id.clone(), cap);
            eligible.push(id);
        }

        let eligible_set: std::collections::BTreeSet<&NodeIdentity> = eligible.iter().collect();

        let mut ranges = BTreeMap::new();
        let mut owner_of = BTreeMap::new();
        let mut origin_owner = BTreeMap::new();
        let mut used: BTreeMap<NodeIdentity, u64> =
            eligible.iter().map(|id| (id.clone(), 0)).collect();
        let mut load: BTreeMap<NodeIdentity, u64> =
            eligible.iter().map(|id| (id.clone(), 0)).collect();
        let mut total_bytes: u64 = 0;

        for entry in ownership.entries() {
            let owner = entry.owner().clone();
            // Only ranges owned by an eligible member are movable here; a draining
            // owner's ranges belong to the drain flow.
            if !eligible_set.contains(&owner) {
                continue;
            }
            let key = (entry.collection().clone(), entry.range_id());
            let load_facts = signals.range_load(entry.collection(), entry.range_id());
            ranges.insert(
                key.clone(),
                RangeFacts {
                    bytes: load_facts.bytes_used,
                    traffic: load_facts.traffic(),
                },
            );
            *used.get_mut(&owner).unwrap() += load_facts.bytes_used;
            *load.get_mut(&owner).unwrap() += load_facts.traffic();
            total_bytes = total_bytes.saturating_add(load_facts.bytes_used);
            owner_of.insert(key.clone(), owner.clone());
            origin_owner.insert(key, owner);
        }

        Self {
            eligible,
            weighted_capacity,
            total_capacity,
            total_bytes,
            ranges,
            owner_of,
            origin_owner,
            used,
            load,
            moved: std::collections::BTreeSet::new(),
        }
    }

    fn fair(&self, member: &NodeIdentity) -> u64 {
        let cap = self.weighted_capacity.get(member).copied().unwrap_or(0);
        fair_share(self.total_bytes, cap, self.total_capacity)
    }

    /// Ranges currently owned by `member` in the simulation, in `(collection,
    /// range_id)` order, that have not already been moved this plan.
    fn ranges_owned_by(&self, member: &NodeIdentity) -> Vec<(CollectionId, RangeId)> {
        self.owner_of
            .iter()
            .filter(|(key, owner)| *owner == member && !self.moved.contains(*key))
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn apply_move(&mut self, key: &(CollectionId, RangeId), to: &NodeIdentity) {
        let facts = self.ranges[key];
        let from = self.owner_of[key].clone();
        *self.used.get_mut(&from).unwrap() -= facts.bytes;
        *self.load.get_mut(&from).unwrap() -= facts.traffic;
        *self.used.get_mut(to).unwrap() += facts.bytes;
        *self.load.get_mut(to).unwrap() += facts.traffic;
        self.owner_of.insert(key.clone(), to.clone());
        self.moved.insert(key.clone());
    }

    /// The **primary** pass: greedily move ranges off members over their
    /// weighted-capacity fair share onto members under theirs, until no member is
    /// over tolerance or no move strictly improves the worst imbalance.
    fn plan_capacity_moves(&mut self, policy: &PlacementPolicy) -> Vec<PlannedMove> {
        let mut planned = Vec::new();
        if self.total_capacity == 0 || self.eligible.len() < 2 {
            return planned;
        }

        // Each range moves at most once, so the loop is bounded by the range count.
        // Pick the member most over its fair share (beyond tolerance) each round,
        // then the member most under its own — the pair whose rebalance helps most.
        while let Some(source) = self.most_over(policy) {
            let Some(target) = self.most_under(&source) else {
                break;
            };

            let dev_src = self.deviation(&source);
            let dev_tgt = self.deviation(&target);
            let worst_before = dev_src.abs().max(dev_tgt.abs());

            // Among the source's still-movable ranges, choose the one that most
            // reduces the worse of the two deviations after the move.
            let mut best: Option<((CollectionId, RangeId), f64)> = None;
            for key in self.ranges_owned_by(&source) {
                let s = self.ranges[&key].bytes as f64;
                let after = (dev_src - s).abs().max((dev_tgt + s).abs());
                let better = match &best {
                    None => true,
                    Some((_, best_after)) => after < *best_after,
                };
                if better {
                    best = Some((key, after));
                }
            }

            let Some((key, worst_after)) = best else {
                break;
            };
            // Only take the move if it strictly improves the worst imbalance —
            // otherwise we would churn ownership for nothing.
            if worst_after >= worst_before {
                break;
            }

            let bytes = self.ranges[&key].bytes;
            let from = self.origin_owner[&key].clone();
            self.apply_move(&key, &target);
            planned.push(PlannedMove {
                collection: key.0,
                range_id: key.1,
                from,
                to: target,
                bytes,
                reason: MoveReason::CapacityBalance,
            });
        }

        planned
    }

    /// A member's deviation from its fair share in bytes: positive is over-full,
    /// negative is under-full.
    fn deviation(&self, member: &NodeIdentity) -> f64 {
        self.used.get(member).copied().unwrap_or(0) as f64 - self.fair(member) as f64
    }

    /// The eligible member furthest over its fair share, beyond the tolerance
    /// band, or `None` if everyone is within tolerance.
    fn most_over(&self, policy: &PlacementPolicy) -> Option<NodeIdentity> {
        self.eligible
            .iter()
            .filter(|id| {
                let used = self.used.get(*id).copied().unwrap_or(0) as f64;
                let fair = self.fair(id) as f64;
                used > fair * (1.0 + policy.balance_tolerance) && used > fair
            })
            .max_by(|a, b| {
                self.deviation(a)
                    .partial_cmp(&self.deviation(b))
                    .unwrap()
                    // Tie-break by identity so the plan is deterministic.
                    .then_with(|| b.cmp(a))
            })
            .cloned()
    }

    /// The eligible member furthest *under* its fair share (the best target),
    /// excluding `source`.
    fn most_under(&self, source: &NodeIdentity) -> Option<NodeIdentity> {
        self.eligible
            .iter()
            .filter(|id| *id != source && self.deviation(id) < 0.0)
            .min_by(|a, b| {
                self.deviation(a)
                    .partial_cmp(&self.deviation(b))
                    .unwrap()
                    // Tie-break by identity so the plan is deterministic.
                    .then_with(|| a.cmp(b))
            })
            .cloned()
    }

    /// The **secondary** pass: identify hotspot ranges (traffic well above the
    /// cluster mean) and, for each, propose spreading it to a member with both
    /// load and capacity headroom — but only when that strictly lowers the owner's
    /// load concentration and respects the capacity tolerance. Returns the
    /// observed hotspots (hottest first) and any relief moves.
    fn plan_hotspot_moves(
        &mut self,
        policy: &PlacementPolicy,
    ) -> (Vec<HotspotRange>, Vec<PlannedMove>) {
        let mut hotspots = Vec::new();
        let mut moves = Vec::new();

        let range_count = self.ranges.len();
        if range_count == 0 {
            return (hotspots, moves);
        }
        let total_traffic: u64 = self.ranges.values().map(|f| f.traffic).sum();
        let mean = total_traffic as f64 / range_count as f64;
        let threshold = mean * policy.hotspot_load_factor;
        if mean <= 0.0 {
            return (hotspots, moves);
        }

        // Collect hotspots, hottest first; tie-break by key for determinism.
        let mut hot: Vec<((CollectionId, RangeId), u64)> = self
            .ranges
            .iter()
            .filter(|(_, f)| f.traffic as f64 > threshold)
            .map(|(key, f)| (key.clone(), f.traffic))
            .collect();
        hot.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        for (key, traffic) in hot {
            let owner = self.owner_of[&key].clone();
            hotspots.push(HotspotRange {
                collection: key.0.clone(),
                range_id: key.1,
                // Report the range's true catalog owner — the member actually
                // bearing the hot traffic — independent of any simulated move.
                owner: self.origin_owner[&key].clone(),
                traffic,
            });

            // A hotspot already scheduled to move (by the capacity pass) needs no
            // second move, and an owner holding only this one range cannot be
            // relieved by moving it — that just relocates the hotspot.
            if self.moved.contains(&key) || self.ranges_owned_by(&owner).len() < 2 {
                continue;
            }

            let facts = self.ranges[&key];
            let owner_load = self.load.get(&owner).copied().unwrap_or(0);

            // Pick the eligible member with the least load that can take the range
            // without breaching its capacity tolerance and ends up less loaded than
            // the owner is now — otherwise the move does not spread load.
            let target = self
                .eligible
                .iter()
                .filter(|id| **id != owner)
                .filter(|id| {
                    let used = self.used.get(*id).copied().unwrap_or(0);
                    let fair = self.fair(id) as f64;
                    (used + facts.bytes) as f64 <= fair * (1.0 + policy.balance_tolerance)
                })
                .filter(|id| {
                    let tgt_load = self.load.get(*id).copied().unwrap_or(0);
                    tgt_load + facts.traffic < owner_load
                })
                .min_by(|a, b| {
                    let la = self.load.get(*a).copied().unwrap_or(0);
                    let lb = self.load.get(*b).copied().unwrap_or(0);
                    la.cmp(&lb).then_with(|| a.cmp(b))
                })
                .cloned();

            if let Some(target) = target {
                let from = self.origin_owner[&key].clone();
                self.apply_move(&key, &target);
                moves.push(PlannedMove {
                    collection: key.0,
                    range_id: key.1,
                    from,
                    to: target,
                    bytes: facts.bytes,
                    reason: MoveReason::HotspotRelief,
                });
            }
        }

        (hotspots, moves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::membership::{ClusterId, ClusterMember, MemberKind};
    use crate::cluster::ownership::{PlacementMetadata, RangeBounds, RangeOwnership, ShardKeyMode};
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

    /// A catalog of `n` single-owner ranges in `orders`, assigning range `i` to
    /// `owners[i]`. Each range is a distinct hash partition so they never overlap.
    fn catalog(owners: &[&str]) -> (ShardOwnershipCatalog, CollectionId) {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        for (i, owner) in owners.iter().enumerate() {
            let lower = vec![i as u8];
            let upper = vec![i as u8 + 1];
            let bounds = RangeBounds::new(
                crate::cluster::ownership::RangeBound::key(lower),
                crate::cluster::ownership::RangeBound::key(upper),
            )
            .unwrap();
            catalog
                .apply_update(RangeOwnership::establish(
                    orders.clone(),
                    RangeId::new(i as u64 + 1),
                    ShardKeyMode::Hash,
                    bounds,
                    ident(owner),
                    Vec::<NodeIdentity>::new(),
                    PlacementMetadata::with_replication_factor(1),
                ))
                .unwrap();
        }
        (catalog, orders)
    }

    /// A scripted [`PlacementSignals`]: per-member capacity (defaulting to a
    /// uniform disk) and per-range load keyed by range id.
    struct FakeSignals {
        default_capacity: MemberCapacity,
        capacity: HashMap<NodeIdentity, MemberCapacity>,
        load: HashMap<u64, RangeLoad>,
        default_bytes: u64,
    }

    impl FakeSignals {
        fn uniform(disk: u64, default_bytes: u64) -> Self {
            Self {
                default_capacity: MemberCapacity::with_disk(disk),
                capacity: HashMap::new(),
                load: HashMap::new(),
                default_bytes,
            }
        }

        fn with_capacity(mut self, cn: &str, cap: MemberCapacity) -> Self {
            self.capacity.insert(ident(cn), cap);
            self
        }

        fn with_load(mut self, range_id: u64, load: RangeLoad) -> Self {
            self.load.insert(range_id, load);
            self
        }
    }

    impl PlacementSignals for FakeSignals {
        fn member_capacity(&self, member: &NodeIdentity) -> MemberCapacity {
            self.capacity
                .get(member)
                .copied()
                .unwrap_or(self.default_capacity)
        }

        fn range_load(&self, _collection: &CollectionId, range_id: RangeId) -> RangeLoad {
            self.load
                .get(&range_id.value())
                .copied()
                .unwrap_or_else(|| RangeLoad::idle(self.default_bytes))
        }
    }

    // --- weighted capacity model -----------------------------------------

    #[test]
    fn weighted_capacity_scales_disk_by_operator_weight() {
        // Neutral weight places strictly by disk.
        assert_eq!(MemberCapacity::with_disk(1_000).weighted_capacity(), 1_000);
        // A 2.0x operator weight doubles the placement weight; 0.5x halves it.
        assert_eq!(MemberCapacity::new(1_000, 200).weighted_capacity(), 2_000);
        assert_eq!(MemberCapacity::new(1_000, 50).weighted_capacity(), 500);
        // No disk -> not placeable.
        assert!(!MemberCapacity::with_disk(0).is_placeable());
        assert!(MemberCapacity::with_disk(1).is_placeable());
    }

    // --- acceptance scenario: homogeneous placement ----------------------

    #[test]
    fn homogeneous_cluster_is_balanced_and_plans_nothing() {
        // Three members, equal disk, three equal-sized ranges one each: already
        // perfectly balanced, so the planner proposes no move.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::uniform(1_000_000, 100);

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert!(plan.is_empty(), "balanced homogeneous cluster is a no-op");
    }

    #[test]
    fn homogeneous_cluster_with_skew_spreads_ranges() {
        // All three ranges sit on node-a while node-b and node-c are empty. With
        // equal capacity the fair share is one range each, so the planner moves two
        // ranges off node-a.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-a", "CN=node-a"]);
        let signals = FakeSignals::uniform(1_000_000, 100);

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert_eq!(
            plan.capacity_moves().count(),
            2,
            "two ranges move off node-a"
        );
        for mv in plan.capacity_moves() {
            assert_eq!(mv.from, ident("CN=node-a"));
            assert_ne!(mv.to, ident("CN=node-a"));
            assert_eq!(mv.reason, MoveReason::CapacityBalance);
        }
        // node-b and node-c each receive exactly one range.
        let targets: std::collections::BTreeSet<_> =
            plan.capacity_moves().map(|m| m.to.clone()).collect();
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn scoped_plan_identifies_the_collection_group_placement_authority() {
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-a", "CN=node-a"]);
        let signals = FakeSignals::uniform(1_000, 100);
        let mut authorities = PlacementAuthorityCatalog::new();
        let group = CollectionGroupId::new("commerce").unwrap();
        let authority = CollectionGroupPlacementAuthority::new(
            group.clone(),
            ident("CN=pa-commerce"),
            [collection("orders"), collection("payments")],
        )
        .unwrap();
        authorities.register(authority).unwrap();

        let plan = planner
            .plan_rebalance_scoped(&members, &catalog, &signals, &authorities)
            .unwrap();

        assert!(
            !plan.moves.is_empty(),
            "skewed ownership should plan movement"
        );
        for planned in &plan.moves {
            assert_eq!(planned.placement_authority.collection_group(), &group);
            assert_eq!(
                planned.placement_authority.authority(),
                &ident("CN=pa-commerce")
            );
            assert_eq!(planned.movement.collection, collection("orders"));
        }
    }

    // --- acceptance scenario: heterogeneous disk weights -----------------

    #[test]
    fn heterogeneous_disk_weights_apportion_by_capacity() {
        // node-big advertises 4x the disk of node-small. Six equal ranges all start
        // on node-small; fair shares are big≈4.8, small≈1.2 ranges, so the planner
        // moves the bulk onto node-big.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-big", "CN=node-small"]);
        let (catalog, _orders) = catalog(&[
            "CN=node-small",
            "CN=node-small",
            "CN=node-small",
            "CN=node-small",
            "CN=node-small",
            "CN=node-small",
        ]);
        let signals = FakeSignals::uniform(1_000, 100)
            .with_capacity("CN=node-big", MemberCapacity::with_disk(4_000))
            .with_capacity("CN=node-small", MemberCapacity::with_disk(1_000));

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert!(!plan.no_moves(), "imbalanced cluster must plan moves");
        // Every move goes from small to big, and big ends with ~4-5 of the 6 ranges.
        let to_big = plan
            .capacity_moves()
            .filter(|m| m.to == ident("CN=node-big"))
            .count();
        assert!(
            (4..=5).contains(&to_big),
            "node-big should receive ~4/5 of 6 ranges, got {to_big}"
        );
        for mv in plan.capacity_moves() {
            assert_eq!(mv.from, ident("CN=node-small"));
            assert_eq!(mv.to, ident("CN=node-big"));
        }
    }

    #[test]
    fn operator_weight_biases_placement_without_more_disk() {
        // Same disk on both, but node-pref carries a 3x operator weight, so it
        // deserves the larger share of four ranges that all start on node-plain.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-pref", "CN=node-plain"]);
        let (catalog, _orders) = catalog(&[
            "CN=node-plain",
            "CN=node-plain",
            "CN=node-plain",
            "CN=node-plain",
        ]);
        let signals = FakeSignals::uniform(1_000, 100)
            .with_capacity("CN=node-pref", MemberCapacity::new(1_000, 300));

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        let to_pref = plan
            .capacity_moves()
            .filter(|m| m.to == ident("CN=node-pref"))
            .count();
        assert!(
            to_pref >= 2,
            "higher operator weight pulls more ranges, got {to_pref}"
        );
    }

    // --- acceptance scenario: capacity expansion -------------------------

    #[test]
    fn expanding_disk_changes_weight_and_next_plan_without_moving_data() {
        // Start heterogeneous: node-a small, node-b large, all six ranges on node-a.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, orders) = catalog(&[
            "CN=node-a",
            "CN=node-a",
            "CN=node-a",
            "CN=node-a",
            "CN=node-a",
            "CN=node-a",
        ]);

        // Before expansion: node-b has only modest disk, so it receives a modest
        // share.
        let before_signals = FakeSignals::uniform(1_000, 100)
            .with_capacity("CN=node-a", MemberCapacity::with_disk(3_000))
            .with_capacity("CN=node-b", MemberCapacity::with_disk(1_000));
        let before = planner.plan_rebalance(&members, &catalog, &before_signals);
        let before_to_b = before
            .capacity_moves()
            .filter(|m| m.to == ident("CN=node-b"))
            .count();

        // Operator expands node-b's disk 8x. Its placement weight jumps...
        let small = MemberCapacity::with_disk(1_000);
        let expanded = MemberCapacity::with_disk(8_000);
        assert!(
            expanded.weighted_capacity() > small.weighted_capacity(),
            "expanding disk raises placement weight",
        );
        let after_signals = FakeSignals::uniform(1_000, 100)
            .with_capacity("CN=node-a", MemberCapacity::with_disk(3_000))
            .with_capacity("CN=node-b", expanded);
        let after = planner.plan_rebalance(&members, &catalog, &after_signals);
        let after_to_b = after
            .capacity_moves()
            .filter(|m| m.to == ident("CN=node-b"))
            .count();

        // ...so the *next* plan apportions more ranges to node-b than before.
        assert!(
            after_to_b > before_to_b,
            "expanded disk pulls more ranges on the next plan ({before_to_b} -> {after_to_b})",
        );

        // But planning never moved data: the catalog still shows all six ranges on
        // node-a. Data only relocates when a transition plan is executed.
        for i in 1..=6 {
            let range = catalog.range(&orders, RangeId::new(i)).unwrap();
            assert_eq!(
                range.owner(),
                &ident("CN=node-a"),
                "range {i} stayed on node-a; planning moved nothing",
            );
        }
    }

    // --- acceptance scenario: hotspot signal influence -------------------

    #[test]
    fn hotspot_traffic_identifies_secondary_candidate() {
        // Capacity is *balanced* — node-a owns two ranges but has twice the disk,
        // so every member sits exactly on its fair share and the primary pass
        // proposes nothing. Yet range 1 on node-a serves a huge read/write load (a
        // small, read-hammered range) while the others are quiet. The secondary
        // signal flags it as a hotspot and, because node-a also carries other
        // traffic and a quiet member has both load and capacity headroom, proposes
        // spreading it.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        // node-a owns ranges 1 and 2; node-b owns 3; node-c owns 4.
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-a", "CN=node-b", "CN=node-c"]);
        // node-a has 2x disk so its fair share covers both its ranges (40 bytes);
        // node-b and node-c each match their single 20-byte range.
        let signals = FakeSignals::uniform(0, 0)
            .with_capacity("CN=node-a", MemberCapacity::with_disk(2_000))
            .with_capacity("CN=node-b", MemberCapacity::with_disk(1_000))
            .with_capacity("CN=node-c", MemberCapacity::with_disk(1_000))
            // The hot range is tiny on disk but hammered; node-a keeps real
            // residual traffic on range 2.
            .with_load(
                1,
                RangeLoad {
                    bytes_used: 2,
                    read_ops: 1_000,
                    write_ops: 1_000,
                },
            )
            .with_load(
                2,
                RangeLoad {
                    bytes_used: 38,
                    read_ops: 300,
                    write_ops: 0,
                },
            )
            .with_load(
                3,
                RangeLoad {
                    bytes_used: 20,
                    read_ops: 100,
                    write_ops: 0,
                },
            )
            .with_load(
                4,
                RangeLoad {
                    bytes_used: 20,
                    read_ops: 100,
                    write_ops: 0,
                },
            );

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        // Capacity is balanced, so no capacity-balance move is proposed.
        assert_eq!(plan.capacity_moves().count(), 0, "capacity is balanced");
        // Range 1 is identified as a hotspot, attributed to its real owner.
        assert_eq!(plan.hotspots.len(), 1, "the hot range is surfaced");
        assert_eq!(plan.hotspots[0].range_id, RangeId::new(1));
        assert_eq!(plan.hotspots[0].owner, ident("CN=node-a"));
        assert_eq!(plan.hotspots[0].traffic, 2_000);
        // And a hotspot-relief move spreads it off node-a onto the quietest member.
        let relief: Vec<_> = plan.hotspot_moves().collect();
        assert_eq!(relief.len(), 1, "a relief move is planned");
        assert_eq!(relief[0].range_id, RangeId::new(1));
        assert_eq!(relief[0].from, ident("CN=node-a"));
        assert_eq!(
            relief[0].to,
            ident("CN=node-b"),
            "quietest target, tie -> lowest id"
        );
        assert_eq!(relief[0].reason, MoveReason::HotspotRelief);
    }

    #[test]
    fn no_hotspot_when_traffic_is_even() {
        // Balanced capacity (one range each, equal disk) and equal traffic: nothing
        // is a hotspot and the secondary signal proposes nothing.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-b", "CN=node-c"]);
        let signals = FakeSignals::uniform(1_000_000, 100)
            .with_load(
                1,
                RangeLoad {
                    bytes_used: 10,
                    read_ops: 100,
                    write_ops: 100,
                },
            )
            .with_load(
                2,
                RangeLoad {
                    bytes_used: 10,
                    read_ops: 100,
                    write_ops: 100,
                },
            )
            .with_load(
                3,
                RangeLoad {
                    bytes_used: 10,
                    read_ops: 100,
                    write_ops: 100,
                },
            );

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert!(plan.is_empty(), "balanced, even-traffic cluster is a no-op");
    }

    // --- acceptance scenario: no implicit data movement ------------------

    #[test]
    fn planning_never_mutates_the_catalog() {
        // A deliberately skewed cluster yields a non-empty plan, yet the ownership
        // catalog is byte-for-byte identical before and after planning: the planner
        // only *describes* moves, it never performs them.
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a", "CN=node-b"]);
        let (catalog, orders) = catalog(&["CN=node-a", "CN=node-a", "CN=node-a", "CN=node-a"]);
        let signals = FakeSignals::uniform(1_000, 100);

        // Snapshot every range's owner/epoch/version before planning.
        let before: Vec<_> = (1..=4)
            .map(|i| {
                let r = catalog.range(&orders, RangeId::new(i)).unwrap();
                (r.owner().clone(), r.epoch(), r.version())
            })
            .collect();

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert!(!plan.no_moves(), "skewed cluster does plan moves");

        // The catalog is unchanged: same owners, same epochs, same versions.
        for (i, snap) in before.iter().enumerate() {
            let r = catalog.range(&orders, RangeId::new(i as u64 + 1)).unwrap();
            assert_eq!(&(r.owner().clone(), r.epoch(), r.version()), snap);
        }
    }

    #[test]
    fn draining_owner_ranges_are_left_to_the_drain_flow() {
        // node-a is draining (not placement-eligible). Its ranges are not moved by
        // the rebalancer — drain owns evacuating them — so no plan targets or
        // sources it for placement balancing.
        let planner = WeightedPlacementPlanner::default();
        let mut members = membership(&["CN=node-a", "CN=node-b"]);
        members.begin_drain(&ident("CN=node-a"));
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-a", "CN=node-a"]);
        let signals = FakeSignals::uniform(1_000, 100);

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        // node-a's ranges are not movable here, and node-b is the only eligible
        // member, so there is nothing to balance.
        assert!(
            plan.no_moves(),
            "draining owner's ranges are not rebalanced"
        );
    }

    #[test]
    fn single_member_cluster_plans_nothing() {
        let planner = WeightedPlacementPlanner::default();
        let members = membership(&["CN=node-a"]);
        let (catalog, _orders) = catalog(&["CN=node-a", "CN=node-a"]);
        let signals = FakeSignals::uniform(1_000, 100);

        let plan = planner.plan_rebalance(&members, &catalog, &signals);
        assert!(
            plan.no_moves(),
            "nowhere to move ranges in a one-member cluster"
        );
    }
}
