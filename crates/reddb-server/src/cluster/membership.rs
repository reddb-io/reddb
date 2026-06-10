//! Cluster member identity, the authorized-member catalog, and the resilient
//! three-data-member baseline (issue #988, PRD #987, ADR 0030).
//!
//! This is the first vertical slice of multi-writer cluster membership. It
//! defines *who is a cluster member* as control-plane state that is distinct
//! from *which ranges a member owns or replicates* (the per-range roles in
//! [`clustering`](../../../.red/context/clustering.md) and ADR 0045). A node
//! has exactly one stable [cluster member identity]; range ownership is a
//! separate, per-range role assigned later by the rebalancer.
//!
//! ## What lives here
//!
//! * [`ClusterId`] — the cluster's own stable identity. A candidate must
//!   present the right cluster id to join; a peer that targets a different
//!   cluster is rejected ([`super::join`]).
//! * [`MemberKind`] — whether a member holds user data ([`MemberKind::Data`])
//!   or is a vote-only witness ([`MemberKind::Witness`]). The resilient
//!   multi-writer baseline counts **data** members; witnesses are not the
//!   recommended baseline (glossary: *Voting member*).
//! * [`ClusterMember`] — one authorized member: its [`NodeIdentity`], its
//!   kind, and how many user ranges it currently holds. A freshly joined data
//!   member holds **zero** ranges — joining never moves user ranges.
//! * [`MembershipCatalog`] — the authorized-member set for one cluster. This
//!   is the *only* set autodetect of health and topology is allowed to range
//!   over: an arbitrary network peer that has not joined is not a member and
//!   is not an autodetect candidate.
//!
//! The join handshake itself — authenticate against a seed, verify cluster
//! identity, reject unknown/unauthorized peers, then admit and hand back the
//! control-plane snapshot — lives in [`super::join`].
//!
//! Everything here is a pure data model with no I/O, so the whole membership
//! and join story is exercised deterministically.

use std::collections::BTreeMap;

use super::identity::NodeIdentity;

/// The resilient baseline for a multi-writer cluster, in **data** members.
///
/// The glossary fixes this: *"A resilient multi-writer cluster starts with
/// three data members; witness members are not the recommended baseline for
/// multi-writer clustering."* Three data members give a quorum of two that
/// survives the loss of any single member without a witness.
pub const RESILIENT_DATA_MEMBER_BASELINE: usize = 3;

/// The cluster's own stable identity.
///
/// Every authorized member agrees on this value, and a join candidate must
/// present it to be admitted (see [`super::join`]). It is what makes a
/// "wrong-cluster" join detectable: a peer that authenticates fine but targets
/// a *different* cluster is rejected, not merged in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClusterId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterIdError;

impl std::fmt::Display for ClusterIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cluster id is empty")
    }
}

impl std::error::Error for ClusterIdError {}

impl ClusterId {
    /// Build a cluster id from an operator-provisioned value. The value must
    /// be non-empty; a blank cluster id would let any peer "match" by
    /// presenting nothing.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ClusterIdError> {
        let value = value.as_ref().trim();
        if value.is_empty() {
            return Err(ClusterIdError);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClusterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a member holds user data or is a vote-only witness.
///
/// This mirrors the election-side `MemberKind` (a witness votes but never owns
/// a range), but it is the *cluster-membership* view: it decides whether a
/// member counts toward the resilient **data-member** baseline. A witness is a
/// member, but it is not a data member, so it does not move the cluster toward
/// [`RESILIENT_DATA_MEMBER_BASELINE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// Holds user data; can be a range owner for some ranges and a range
    /// replica for others.
    Data,
    /// Control-plane only; stores no user data and is never a range owner.
    Witness,
}

impl MemberKind {
    /// Does this member kind store user data (and therefore count toward the
    /// resilient multi-writer baseline)?
    pub fn holds_data(self) -> bool {
        matches!(self, MemberKind::Data)
    }
}

/// A member's lifecycle state in the cluster (issue #1000, PRD #987).
///
/// A member is [`Active`](Self::Active) for its whole serving life; planned
/// removal first marks it [`Draining`](Self::Draining) via
/// [`MembershipCatalog::begin_drain`]. The distinction drives two rules of the
/// cluster drain flow ([`super::drain`]): a draining member stops receiving new
/// range placements, and its ranges are scheduled off it through ordinary
/// ownership transitions before membership is finally removed. The state is
/// *cluster-membership* lifecycle, separate from per-range health
/// ([`HealthClass`](super::supervisor::HealthClass)): a draining member can be
/// perfectly healthy, and an unhealthy member is not automatically draining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberState {
    /// Fully serving: may own and replicate ranges and receive new placements.
    Active,
    /// Marked for planned removal: holds its current ranges until they are moved
    /// off, but receives **no** new range placements. The terminal state before
    /// the member is removed from the catalog.
    Draining,
}

impl MemberState {
    /// Whether a member in this state may receive *new* range placements. Only an
    /// [`Active`](Self::Active) member may; a draining member is excluded so drain
    /// never has to chase ranges it just handed back. This is the "a draining
    /// member stops receiving new range placements" rule.
    pub fn accepts_new_placements(self) -> bool {
        matches!(self, MemberState::Active)
    }
}

/// One authorized cluster member.
///
/// The [`NodeIdentity`] is the member's stable cluster identity — the same
/// validated X.509 subject it authenticates and votes under. `owned_range_count`
/// is the *per-range* role count, kept deliberately separate: a member's
/// cluster identity does not change when ranges move on or off it, and a
/// freshly joined data member starts at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMember {
    identity: NodeIdentity,
    kind: MemberKind,
    state: MemberState,
    owned_range_count: usize,
}

impl ClusterMember {
    /// A member as it exists immediately after a successful join: authorized,
    /// [`Active`](MemberState::Active), of the granted kind, and holding **no**
    /// user ranges. Ranges are only assigned later by rebalancing or ownership
    /// transitions.
    pub fn joined_empty(identity: NodeIdentity, kind: MemberKind) -> Self {
        Self {
            identity,
            kind,
            state: MemberState::Active,
            owned_range_count: 0,
        }
    }

    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    pub fn kind(&self) -> MemberKind {
        self.kind
    }

    /// This member's lifecycle state ([`Active`](MemberState::Active) or
    /// [`Draining`](MemberState::Draining)).
    pub fn state(&self) -> MemberState {
        self.state
    }

    /// Is this member draining (marked for planned removal)?
    pub fn is_draining(&self) -> bool {
        self.state == MemberState::Draining
    }

    /// Mark this member draining. Idempotent: re-marking a draining member is a
    /// no-op. Returns whether the state changed (false if it was already
    /// draining), so a caller can tell a fresh drain from a repeated request.
    pub fn begin_drain(&mut self) -> bool {
        let changed = self.state == MemberState::Active;
        self.state = MemberState::Draining;
        changed
    }

    /// Whether this member may receive *new* range placements: only an active
    /// data member can. A witness never holds user data, and a draining member is
    /// being emptied, so neither is a placement target.
    pub fn is_placement_eligible(&self) -> bool {
        self.kind.holds_data() && self.state.accepts_new_placements()
    }

    /// How many user ranges this member currently owns. Distinct from cluster
    /// membership: a member with zero ranges is still a full member.
    pub fn owned_range_count(&self) -> usize {
        self.owned_range_count
    }

    /// Does this member currently hold any user ranges? A just-joined member
    /// answers `false` until the rebalancer assigns ownership.
    pub fn holds_user_ranges(&self) -> bool {
        self.owned_range_count > 0
    }

    /// Record that the rebalancer/ownership transitions have assigned this many
    /// user ranges to the member. This is the *only* path that gives a member
    /// ranges — join never does.
    pub fn assign_ranges(&mut self, count: usize) {
        self.owned_range_count = count;
    }
}

/// How a candidate compared against the authorized-member set on join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOutcome {
    /// The candidate was not previously a member and was admitted now.
    Admitted,
    /// The candidate was already an authorized member; the catalog is
    /// unchanged (join is idempotent on reconnect).
    AlreadyMember,
}

/// The authorized-member set for one cluster — the control-plane membership
/// catalog.
///
/// Membership is explicit: a node appears here only after a successful join
/// ([`super::join`]). Autodetect of health and topology ranges over
/// [`autodetect_candidates`](Self::autodetect_candidates) — i.e. *these
/// members only* — never over arbitrary peers that happen to be reachable on
/// the network.
#[derive(Debug, Clone)]
pub struct MembershipCatalog {
    cluster_id: ClusterId,
    members: BTreeMap<NodeIdentity, ClusterMember>,
}

impl MembershipCatalog {
    /// A catalog for `cluster_id` seeded with `founders`. The founding data
    /// members are the bootstrap set that later candidates authenticate
    /// against; each starts empty.
    pub fn new(cluster_id: ClusterId, founders: impl IntoIterator<Item = ClusterMember>) -> Self {
        let members = founders
            .into_iter()
            .map(|m| (m.identity().clone(), m))
            .collect();
        Self {
            cluster_id,
            members,
        }
    }

    pub fn cluster_id(&self) -> &ClusterId {
        &self.cluster_id
    }

    /// Is `identity` an authorized member of this cluster? This is the gate
    /// every control-plane path consults — only an authorized member's health
    /// and topology are autodetected, and only a member may vote or own ranges.
    pub fn is_authorized(&self, identity: &NodeIdentity) -> bool {
        self.members.contains_key(identity)
    }

    pub fn member(&self, identity: &NodeIdentity) -> Option<&ClusterMember> {
        self.members.get(identity)
    }

    pub fn member_mut(&mut self, identity: &NodeIdentity) -> Option<&mut ClusterMember> {
        self.members.get_mut(identity)
    }

    /// Admit `member` as authorized. Idempotent: re-admitting an existing
    /// member leaves the catalog (and the member's range count) untouched, so
    /// a reconnecting member never has its ranges reset to zero.
    pub fn admit(&mut self, member: ClusterMember) -> AdmissionOutcome {
        if self.members.contains_key(member.identity()) {
            return AdmissionOutcome::AlreadyMember;
        }
        self.members.insert(member.identity().clone(), member);
        AdmissionOutcome::Admitted
    }

    /// Mark an authorized member draining (planned-removal flow, issue #1000).
    /// Returns `None` if `identity` is not a member, otherwise whether the state
    /// changed (false if it was already draining). A draining member keeps its
    /// ranges until drain moves them off, but is no longer a placement target.
    pub fn begin_drain(&mut self, identity: &NodeIdentity) -> Option<bool> {
        self.members
            .get_mut(identity)
            .map(ClusterMember::begin_drain)
    }

    /// Remove a member from the authorized set, returning the removed
    /// [`ClusterMember`] (or `None` if it was not a member). This is the final
    /// step of both the planned drain and the force-remove flows; callers gate it
    /// on the range-dependency checks in [`super::drain`] — the catalog itself
    /// does not re-check, so a force remove of a dead member can drop it even
    /// while ranges still nominally list it.
    pub fn remove(&mut self, identity: &NodeIdentity) -> Option<ClusterMember> {
        self.members.remove(identity)
    }

    /// Every authorized member, in stable identity order.
    pub fn members(&self) -> impl Iterator<Item = &ClusterMember> {
        self.members.values()
    }

    /// The members eligible to receive *new* range placements — active data
    /// members only, in stable identity order. Draining members and witnesses are
    /// excluded, so a rebalancer or a drain's replica-evacuation never targets a
    /// member that is itself on the way out.
    pub fn placement_eligible_members(&self) -> impl Iterator<Item = &ClusterMember> {
        self.members().filter(|m| m.is_placement_eligible())
    }

    /// The members autodetect of health/topology is allowed to range over —
    /// exactly the authorized members. An arbitrary network peer that has not
    /// joined is absent here, so autodetect can never silently adopt it.
    pub fn autodetect_candidates(&self) -> impl Iterator<Item = &ClusterMember> {
        self.members()
    }

    /// Whether autodetect may consider `identity`. True only for authorized
    /// members — the rule that "autodetect applies only to authorized members
    /// after join, not arbitrary network peers".
    pub fn is_autodetect_eligible(&self, identity: &NodeIdentity) -> bool {
        self.is_authorized(identity)
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// How many **data** members the cluster currently has (witnesses
    /// excluded). This is the number the resilient baseline is measured in.
    pub fn data_member_count(&self) -> usize {
        self.members().filter(|m| m.kind().holds_data()).count()
    }

    /// Assess the cluster against the resilient multi-writer baseline of
    /// [`RESILIENT_DATA_MEMBER_BASELINE`] data members.
    pub fn assess_baseline(&self) -> BaselineAssessment {
        BaselineAssessment::evaluate(self.data_member_count())
    }
}

/// How the cluster's data-member count compares to the resilient baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineAssessment {
    /// The configured resilient baseline ([`RESILIENT_DATA_MEMBER_BASELINE`]).
    pub recommended_data_members: usize,
    /// The cluster's current data-member count.
    pub data_members: usize,
}

impl BaselineAssessment {
    fn evaluate(data_members: usize) -> Self {
        Self {
            recommended_data_members: RESILIENT_DATA_MEMBER_BASELINE,
            data_members,
        }
    }

    /// Does the cluster meet (or exceed) the resilient multi-writer baseline?
    pub fn meets_baseline(&self) -> bool {
        self.data_members >= self.recommended_data_members
    }

    /// How many more data members are needed to reach the baseline (zero once
    /// met).
    pub fn shortfall(&self) -> usize {
        self.recommended_data_members
            .saturating_sub(self.data_members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn data_member(cn: &str) -> ClusterMember {
        ClusterMember::joined_empty(ident(cn), MemberKind::Data)
    }

    #[test]
    fn cluster_id_rejects_empty() {
        assert!(ClusterId::new("   ").is_err());
        assert_eq!(ClusterId::new(" cluster-x ").unwrap().as_str(), "cluster-x");
    }

    #[test]
    fn member_identity_is_distinct_from_range_ownership() {
        // A member's cluster identity is stable; assigning/removing ranges is a
        // separate per-range role and does not change membership.
        let mut m = data_member("CN=node-a");
        assert!(!m.holds_user_ranges());
        assert_eq!(m.owned_range_count(), 0);

        m.assign_ranges(4);
        assert!(m.holds_user_ranges());
        assert_eq!(m.identity(), &ident("CN=node-a")); // identity unchanged
    }

    #[test]
    fn data_member_count_excludes_witnesses() {
        let cid = ClusterId::new("cluster-x").unwrap();
        let catalog = MembershipCatalog::new(
            cid,
            [
                data_member("CN=node-a"),
                data_member("CN=node-b"),
                ClusterMember::joined_empty(ident("CN=witness"), MemberKind::Witness),
            ],
        );
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog.data_member_count(), 2);
    }

    #[test]
    fn three_data_members_meet_resilient_baseline() {
        let cid = ClusterId::new("cluster-x").unwrap();
        let catalog = MembershipCatalog::new(
            cid,
            [
                data_member("CN=node-a"),
                data_member("CN=node-b"),
                data_member("CN=node-c"),
            ],
        );
        let baseline = catalog.assess_baseline();
        assert_eq!(baseline.recommended_data_members, 3);
        assert!(baseline.meets_baseline());
        assert_eq!(baseline.shortfall(), 0);
    }

    #[test]
    fn two_data_plus_witness_does_not_meet_baseline() {
        // A witness is not the recommended baseline: 2 data + 1 witness is
        // below the three-data-member baseline.
        let cid = ClusterId::new("cluster-x").unwrap();
        let catalog = MembershipCatalog::new(
            cid,
            [
                data_member("CN=node-a"),
                data_member("CN=node-b"),
                ClusterMember::joined_empty(ident("CN=witness"), MemberKind::Witness),
            ],
        );
        let baseline = catalog.assess_baseline();
        assert!(!baseline.meets_baseline());
        assert_eq!(baseline.shortfall(), 1);
    }

    #[test]
    fn admit_is_idempotent_and_preserves_ranges() {
        let cid = ClusterId::new("cluster-x").unwrap();
        let mut catalog = MembershipCatalog::new(cid, [data_member("CN=node-a")]);
        catalog
            .member_mut(&ident("CN=node-a"))
            .unwrap()
            .assign_ranges(3);

        // Re-admitting must not reset an existing member's range count.
        let outcome = catalog.admit(data_member("CN=node-a"));
        assert_eq!(outcome, AdmissionOutcome::AlreadyMember);
        assert_eq!(
            catalog
                .member(&ident("CN=node-a"))
                .unwrap()
                .owned_range_count(),
            3
        );

        let outcome = catalog.admit(data_member("CN=node-b"));
        assert_eq!(outcome, AdmissionOutcome::Admitted);
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn autodetect_is_limited_to_authorized_members() {
        let cid = ClusterId::new("cluster-x").unwrap();
        let catalog = MembershipCatalog::new(cid, [data_member("CN=node-a")]);

        // An authorized member is an autodetect candidate.
        assert!(catalog.is_autodetect_eligible(&ident("CN=node-a")));
        // An arbitrary reachable network peer that never joined is not.
        assert!(!catalog.is_autodetect_eligible(&ident("CN=random-peer")));
        assert_eq!(catalog.autodetect_candidates().count(), 1);
    }
}
