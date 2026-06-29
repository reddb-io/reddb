//! Ownership leases and owner self-fence behavior (issue #997, PRD #987,
//! ADR 0037).
//!
//! The [`ShardOwnershipCatalog`] (issue #989) records *who* owns a range and the
//! [ownership-transition machine](super::ownership_transition) is the only
//! sanctioned way to *move* that authority. But catalog ownership alone is not
//! enough to make a durable write safe: a node that the catalog still names as
//! owner may have been partitioned away from the Cluster Supervisor, so the rest
//! of the cluster has already moved on without it being able to learn so. The
//! ownership *lease* closes that gap.
//!
//! Per the glossary an **ownership lease** is *"time-bounded authority for a range
//! owner to accept durable writes, issued under the current Cluster Supervisor
//! term and ownership epoch. If the Supervisor loses majority, owners may continue
//! only until their valid lease expires."* The lease is the owner's *positive*
//! permission to write — without a currently-valid one it must stop, even if the
//! catalog still names it owner and nothing has explicitly deposed it.
//!
//! ## What a lease binds together
//!
//! An [`OwnershipLease`] ties four identities together, matching ADR 0037's
//! "expected term and ownership epoch" fencing inputs plus the range and owner the
//! authority is *for*:
//!
//! * [`SupervisorTerm`] — the Cluster Supervisor term the lease was granted under.
//!   When the Supervisor term advances (a new Supervisor leader), an old lease no
//!   longer matches and the owner self-fences.
//! * [`CollectionId`] + [`RangeId`] — the single range this authority covers. A
//!   lease is per-range, exactly like [`RangeRole`].
//! * `owner` ([`NodeIdentity`]) — the node the authority was issued to.
//! * [`OwnershipEpoch`] — the ownership epoch in force when the lease was granted.
//!   If ownership moves (the epoch bumps via a [transition](super::ownership_transition)),
//!   the lease no longer matches the catalog and the owner self-fences.
//!
//! plus a `[granted_at_ms, expires_at_ms)` validity window — the *time-bounded*
//! part. Time is passed in explicitly (`now_ms`) so the whole module stays a pure,
//! deterministic data model with no clock I/O, just like its siblings.
//!
//! ## Self-fence and read-only mode
//!
//! [`LeasedOwner`] is the owner's local view of its own lease. It answers one
//! question — *may I take a durable write right now?* — by [`evaluate`] against the
//! current Supervisor term, the range's current ownership epoch, and the current
//! time. The answer is an [`OwnerWriteMode`]: either [`Durable`] (a valid lease
//! covers the write) or [`Fenced`] with the [`FenceReason`] that revoked authority.
//! An owner self-fences — per the glossary's **owner self-fence** — when its lease
//! *expires*, is *revoked*, or no longer matches the current *Supervisor term* or
//! *ownership epoch*; it does not wait for clients to stop routing to it.
//!
//! A fenced owner is not dead: per the glossary's **self-fenced read mode** it
//! *"may continue serving explicitly stale/read-only requests and replication
//! catch-up, while rejecting durable writes until quorum/lease authority is
//! restored."* [`admit_request`](LeasedOwner::admit_request) encodes exactly that —
//! a [`DurableWrite`](RangeRequest::DurableWrite) is rejected once fenced, while a
//! [`StaleRead`](RangeRequest::StaleRead) and
//! [`ReplicationCatchUp`](RangeRequest::ReplicationCatchUp) are still served.
//!
//! ## Lease *in addition to* ownership
//!
//! [`admit_durable_write`] is the combined gate the public write path calls: it
//! first routes the key and checks catalog ownership (the [`RangeRole`] gate from
//! issue #990), then requires a valid lease on top. A node that is the catalog
//! owner but holds no current lease is rejected — "durable writes require a valid
//! current ownership lease *in addition to* matching range ownership".
//!
//! [`evaluate`]: LeasedOwner::evaluate
//! [`Durable`]: OwnerWriteMode::Durable
//! [`Fenced`]: OwnerWriteMode::Fenced

use super::identity::NodeIdentity;
use super::ownership::{
    CollectionId, OwnershipEpoch, RangeId, RangeOwnership, RangeRole, ShardOwnershipCatalog,
};

/// The Cluster Supervisor term an ownership lease is granted under.
///
/// A lease is authority *"issued under the current Cluster Supervisor term"*: when
/// a new Supervisor leader is elected the term advances, and a lease stamped with
/// an older term no longer matches — its holder self-fences
/// ([`FenceReason::TermSuperseded`]). This is the control-plane analogue of the
/// replication term that fences a deposed primary (ADR 0030).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SupervisorTerm(u64);

impl SupervisorTerm {
    /// The term a freshly-bootstrapped Supervisor starts at.
    pub fn genesis() -> Self {
        Self(1)
    }

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// The next term, as minted when a new Supervisor leader is elected.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for SupervisorTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Time-bounded write authority for one range owner, issued under a Supervisor
/// term and ownership epoch.
///
/// A lease is the owner's *positive* permission to take durable writes. It is
/// per-range (it names its [`CollectionId`] + [`RangeId`]), bound to the owner it
/// was issued to, and valid only on the `[granted_at_ms, expires_at_ms)` window
/// and only while the Supervisor term and ownership epoch it carries still match
/// the live cluster. The owner re-validates it on every durable write through
/// [`LeasedOwner::evaluate`]; once any binding no longer holds, the owner
/// self-fences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipLease {
    supervisor_term: SupervisorTerm,
    collection: CollectionId,
    range_id: RangeId,
    owner: NodeIdentity,
    epoch: OwnershipEpoch,
    granted_at_ms: u64,
    expires_at_ms: u64,
}

impl OwnershipLease {
    /// Grant a lease valid for `ttl_ms` from `granted_at_ms`, under
    /// `supervisor_term` and ownership `epoch`, for `owner`'s authority over
    /// `(collection, range_id)`.
    #[allow(clippy::too_many_arguments)]
    pub fn grant(
        supervisor_term: SupervisorTerm,
        collection: CollectionId,
        range_id: RangeId,
        owner: NodeIdentity,
        epoch: OwnershipEpoch,
        granted_at_ms: u64,
        ttl_ms: u64,
    ) -> Self {
        Self {
            supervisor_term,
            collection,
            range_id,
            owner,
            epoch,
            granted_at_ms,
            expires_at_ms: granted_at_ms.saturating_add(ttl_ms),
        }
    }

    pub fn supervisor_term(&self) -> SupervisorTerm {
        self.supervisor_term
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }

    pub fn granted_at_ms(&self) -> u64 {
        self.granted_at_ms
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// Has the lease's validity window closed at `now_ms`? The window is
    /// half-open: the instant `now_ms == expires_at_ms` is already expired, so a
    /// lease never grants authority at or past its stated end.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// Milliseconds of authority left at `now_ms`, saturating to zero once
    /// expired. The owner's keep-alive uses this to decide when to renew.
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        self.expires_at_ms.saturating_sub(now_ms)
    }

    /// Does this lease cover the range `(collection, range_id)` for `owner`? A
    /// lease is per-range and per-owner, so a held lease for a *different* range
    /// or issued to a *different* owner does not authorise this one.
    fn covers(&self, collection: &CollectionId, range_id: RangeId, owner: &NodeIdentity) -> bool {
        self.collection == *collection && self.range_id == range_id && self.owner == *owner
    }
}

/// Why a range owner is self-fenced — the cause that revoked its durable-write
/// authority. Reported by [`LeasedOwner::evaluate`] and carried in every
/// durable-write rejection so an operator (or the owner's own logs) can see
/// *which* binding lapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceReason {
    /// The owner holds no lease at all (never granted one, or it was dropped).
    /// Catalog ownership without a lease is not authority to write.
    Unleased,
    /// The Supervisor explicitly revoked the lease before its window closed —
    /// e.g. ahead of a planned ownership handoff.
    Revoked,
    /// The lease was granted under an older Supervisor term than the current one:
    /// a new Supervisor leader has been elected, so the lease no longer matches.
    TermSuperseded {
        lease_term: SupervisorTerm,
        current_term: SupervisorTerm,
    },
    /// The lease's ownership epoch no longer matches the range's current epoch:
    /// ownership has moved via a transition, fencing this (now stale) owner.
    EpochSuperseded {
        lease_epoch: OwnershipEpoch,
        current_epoch: OwnershipEpoch,
    },
    /// The lease's validity window has closed at the current time. With the
    /// Supervisor partitioned away the owner cannot renew, so it stops writing
    /// the moment the lease lapses.
    Expired { now_ms: u64, expires_at_ms: u64 },
}

impl std::fmt::Display for FenceReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unleased => write!(f, "owner holds no ownership lease"),
            Self::Revoked => write!(f, "ownership lease was revoked"),
            Self::TermSuperseded {
                lease_term,
                current_term,
            } => write!(
                f,
                "ownership lease granted under supervisor term {lease_term} is behind current term {current_term}"
            ),
            Self::EpochSuperseded {
                lease_epoch,
                current_epoch,
            } => write!(
                f,
                "ownership lease epoch {lease_epoch} no longer matches current ownership epoch {current_epoch}"
            ),
            Self::Expired {
                now_ms,
                expires_at_ms,
            } => write!(
                f,
                "ownership lease expired at {expires_at_ms} ms (now {now_ms} ms)"
            ),
        }
    }
}

impl std::error::Error for FenceReason {}

/// The owner's durable-write authority after evaluating its lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerWriteMode {
    /// A valid lease covers the write — durable writes are authorised.
    Durable,
    /// The owner is self-fenced: durable writes are rejected, but
    /// [`self-fenced read mode`](LeasedOwner::admit_request) still serves stale
    /// reads and replication catch-up. Carries the [`FenceReason`].
    Fenced(FenceReason),
}

impl OwnerWriteMode {
    /// Whether durable writes are authorised in this mode.
    pub fn may_write_durable(&self) -> bool {
        matches!(self, OwnerWriteMode::Durable)
    }

    /// Whether the owner is self-fenced.
    pub fn is_fenced(&self) -> bool {
        matches!(self, OwnerWriteMode::Fenced(_))
    }
}

/// A request kind a (possibly self-fenced) range owner may be asked to serve.
///
/// The distinction drives [`self-fenced read mode`](LeasedOwner::admit_request):
/// a fenced owner rejects [`DurableWrite`](Self::DurableWrite) but still serves
/// [`StaleRead`](Self::StaleRead) and [`ReplicationCatchUp`](Self::ReplicationCatchUp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// A durable mutation. Requires a currently-valid lease.
    DurableWrite,
    /// An explicitly stale / read-only request. Served even while self-fenced.
    StaleRead,
    /// Replication catch-up (a replica streaming the range's log forward).
    /// Served even while self-fenced — it is the very mechanism by which the
    /// member rejoins under a newer ownership epoch.
    ReplicationCatchUp,
}

/// Why a request was refused while the owner is self-fenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFenceRejection {
    /// The fence cause that was in effect when the request was refused.
    pub reason: FenceReason,
}

impl std::fmt::Display for LeaseFenceRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "durable write rejected: owner is self-fenced ({})",
            self.reason
        )
    }
}

impl std::error::Error for LeaseFenceRejection {}

/// A range owner's local view of its own ownership lease — the thing that decides
/// whether it may take a durable write *right now*.
///
/// This is the home of owner self-fence behavior. It holds at most one lease (a
/// lease is per-range, so one [`LeasedOwner`] tracks one owned range) plus a
/// `revoked` flag the Supervisor can trip. [`evaluate`](Self::evaluate) folds the
/// lease, the revoke flag, the current Supervisor term, the range's current
/// ownership epoch, and the current time into an [`OwnerWriteMode`]; everything
/// else is built on that one decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeasedOwner {
    lease: Option<OwnershipLease>,
    revoked: bool,
}

impl LeasedOwner {
    /// An owner holding no lease — self-fenced until one is granted.
    pub fn unleased() -> Self {
        Self {
            lease: None,
            revoked: false,
        }
    }

    /// An owner holding `lease`.
    pub fn with_lease(lease: OwnershipLease) -> Self {
        Self {
            lease: Some(lease),
            revoked: false,
        }
    }

    /// Install a freshly-granted (or renewed) lease, clearing any prior revoke.
    /// Renewing is how the owner extends its window before the old one expires.
    pub fn grant(&mut self, lease: OwnershipLease) {
        self.lease = Some(lease);
        self.revoked = false;
    }

    /// Revoke the current lease. The owner self-fences immediately on its next
    /// [`evaluate`](Self::evaluate), without waiting for the window to close —
    /// this is the Supervisor's explicit "stop writing now" ahead of a handoff.
    pub fn revoke(&mut self) {
        self.revoked = true;
    }

    /// The lease currently held, if any. `None` once revoked-and-dropped or never
    /// granted; note a *held-but-invalid* lease (expired, stale term/epoch) still
    /// returns `Some` here — validity is [`evaluate`](Self::evaluate)'s job.
    pub fn lease(&self) -> Option<&OwnershipLease> {
        self.lease.as_ref()
    }

    /// Decide the owner's durable-write authority against the current control
    /// plane. The lease must be present and un-revoked, granted under the current
    /// `current_term`, carry the range's current `current_epoch`, and still be
    /// inside its validity window at `now_ms`. Any failure self-fences with the
    /// corresponding [`FenceReason`].
    ///
    /// Checks run fail-closed in order of authority: an explicit revoke first,
    /// then absence of a lease, then Supervisor-term supersession, then ownership
    /// epoch supersession, then time expiry. The first cause that holds is the
    /// one reported.
    pub fn evaluate(
        &self,
        current_term: SupervisorTerm,
        current_epoch: OwnershipEpoch,
        now_ms: u64,
    ) -> OwnerWriteMode {
        if self.revoked {
            return OwnerWriteMode::Fenced(FenceReason::Revoked);
        }
        let Some(lease) = &self.lease else {
            return OwnerWriteMode::Fenced(FenceReason::Unleased);
        };
        if lease.supervisor_term != current_term {
            return OwnerWriteMode::Fenced(FenceReason::TermSuperseded {
                lease_term: lease.supervisor_term,
                current_term,
            });
        }
        if lease.epoch != current_epoch {
            return OwnerWriteMode::Fenced(FenceReason::EpochSuperseded {
                lease_epoch: lease.epoch,
                current_epoch,
            });
        }
        if lease.is_expired(now_ms) {
            return OwnerWriteMode::Fenced(FenceReason::Expired {
                now_ms,
                expires_at_ms: lease.expires_at_ms,
            });
        }
        OwnerWriteMode::Durable
    }

    /// Admit (or refuse) a request in light of the owner's current mode — the
    /// encoding of **self-fenced read mode**. A [`DurableWrite`] needs a valid
    /// lease; a [`StaleRead`] and [`ReplicationCatchUp`] are served regardless,
    /// so a fenced owner keeps answering reads and catching up replicas while it
    /// rejects durable writes.
    ///
    /// [`DurableWrite`]: RangeRequest::DurableWrite
    /// [`StaleRead`]: RangeRequest::StaleRead
    /// [`ReplicationCatchUp`]: RangeRequest::ReplicationCatchUp
    pub fn admit_request(
        &self,
        request: RangeRequest,
        current_term: SupervisorTerm,
        current_epoch: OwnershipEpoch,
        now_ms: u64,
    ) -> Result<(), LeaseFenceRejection> {
        match self.evaluate(current_term, current_epoch, now_ms) {
            OwnerWriteMode::Durable => Ok(()),
            OwnerWriteMode::Fenced(reason) => match request {
                RangeRequest::StaleRead | RangeRequest::ReplicationCatchUp => Ok(()),
                RangeRequest::DurableWrite => Err(LeaseFenceRejection { reason }),
            },
        }
    }
}

/// Why a lease-gated durable write was rejected — either the catalog ownership
/// gate refused it (routing / not-owner), or the owner is self-fenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableWriteReject {
    /// No range of the collection covers the routed key — re-resolve routing.
    NoRange { collection: CollectionId },
    /// This node previously held ownership for the range, but the catalog has
    /// advanced to a newer owner/epoch. Carries both ownership identities and
    /// epochs so callers can distinguish stale ownership from an ordinary
    /// non-owner write.
    StaleOwnership {
        collection: CollectionId,
        range_id: RangeId,
        attempted_owner: NodeIdentity,
        current_owner: NodeIdentity,
        attempted_epoch: OwnershipEpoch,
        current_epoch: OwnershipEpoch,
    },
    /// This node is not the catalog owner of the routed range (it is a replica or
    /// holds no copy). The write must be routed to `owner`.
    NotOwner {
        collection: CollectionId,
        range_id: RangeId,
        role: RangeRole,
        owner: NodeIdentity,
    },
    /// This node *is* the catalog owner, but it is self-fenced: it holds no valid
    /// lease for the range. Carries the [`FenceReason`].
    Fenced {
        collection: CollectionId,
        range_id: RangeId,
        reason: FenceReason,
    },
}

impl std::fmt::Display for DurableWriteReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRange { collection } => write!(
                f,
                "no range of collection {collection} covers the routed key — re-resolve routing"
            ),
            Self::StaleOwnership {
                collection,
                range_id,
                attempted_owner,
                current_owner,
                attempted_epoch,
                current_epoch,
            } => write!(
                f,
                "stale ownership for {collection}/{range_id}: {attempted_owner} tried epoch {attempted_epoch}, current owner is {current_owner} at epoch {current_epoch}"
            ),
            Self::NotOwner {
                collection,
                range_id,
                owner,
                ..
            } => write!(
                f,
                "this node does not own {collection}/{range_id} — route the durable write to {owner}"
            ),
            Self::Fenced {
                collection,
                range_id,
                reason,
            } => write!(
                f,
                "owner of {collection}/{range_id} is self-fenced and rejects the durable write: {reason}"
            ),
        }
    }
}

impl std::error::Error for DurableWriteReject {}

/// The combined durable-write gate: catalog ownership **and** a valid lease.
///
/// Routes `key` to its range, requires `node` to be the range's current
/// [`Owner`](RangeRole::Owner) (the issue #990 gate), then requires `holder` to
/// hold a lease that covers this range and is valid at `current_term` /
/// `now_ms` against the range's current ownership epoch. On success returns the
/// owned [`RangeOwnership`]; otherwise the [`DurableWriteReject`] explaining which
/// layer refused.
///
/// This is the literal encoding of the acceptance criterion *"durable writes
/// require a valid current ownership lease in addition to matching range
/// ownership"*: catalog ownership is necessary but not sufficient, and the lease
/// is checked against the catalog's *current* epoch, so an owner whose lease epoch
/// has been superseded by a transition is fenced here too.
pub fn admit_durable_write<'c>(
    catalog: &'c ShardOwnershipCatalog,
    holder: &LeasedOwner,
    node: &NodeIdentity,
    collection: &CollectionId,
    key: &[u8],
    current_term: SupervisorTerm,
    now_ms: u64,
) -> Result<&'c RangeOwnership, DurableWriteReject> {
    let range =
        catalog
            .route_shard_key(collection, key)
            .ok_or_else(|| DurableWriteReject::NoRange {
                collection: collection.clone(),
            })?;

    let role = range.role_of(node);
    let held_lease = holder.lease().filter(|lease| {
        lease.collection() == collection
            && lease.range_id() == range.range_id()
            && lease.owner() == node
    });
    if !role.may_write_public() {
        if let Some(lease) = held_lease {
            if lease.epoch() < range.epoch() {
                return Err(DurableWriteReject::StaleOwnership {
                    collection: collection.clone(),
                    range_id: range.range_id(),
                    attempted_owner: node.clone(),
                    current_owner: range.owner().clone(),
                    attempted_epoch: lease.epoch(),
                    current_epoch: range.epoch(),
                });
            }
        }
        return Err(DurableWriteReject::NotOwner {
            collection: collection.clone(),
            range_id: range.range_id(),
            role,
            owner: range.owner().clone(),
        });
    }

    // The lease must cover *this* range and *this* owner; a held lease for a
    // different range or owner does not authorise the write (treated as unleased
    // for this range).
    let covered = held_lease.is_some();

    let mode = if covered {
        holder.evaluate(current_term, range.epoch(), now_ms)
    } else {
        OwnerWriteMode::Fenced(FenceReason::Unleased)
    };

    match mode {
        OwnerWriteMode::Durable => Ok(range),
        OwnerWriteMode::Fenced(reason) => Err(DurableWriteReject::Fenced {
            collection: collection.clone(),
            range_id: range.range_id(),
            reason,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{PlacementMetadata, RangeBounds, ShardKeyMode};

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    /// A catalog holding one full-keyspace range owned by `owner` with `replicas`.
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

    /// The ownership epoch a single `transfer_to` advances past the initial one
    /// (value 2) — obtained without the crate-private `OwnershipEpoch::next`.
    fn next_epoch() -> OwnershipEpoch {
        RangeOwnership::establish(
            collection("orders"),
            RangeId::new(1),
            ShardKeyMode::Hash,
            RangeBounds::full(),
            ident("CN=node-a"),
            [ident("CN=node-b")],
            PlacementMetadata::with_replication_factor(3),
        )
        .transfer_to(ident("CN=node-b"), [])
        .epoch()
    }

    /// A lease for `owner` over orders/1 under term 1, epoch 1, granted at t=0 for
    /// `ttl_ms`.
    fn lease_for(orders: &CollectionId, owner: &str, ttl_ms: u64) -> OwnershipLease {
        OwnershipLease::grant(
            SupervisorTerm::genesis(),
            orders.clone(),
            RangeId::new(1),
            ident(owner),
            OwnershipEpoch::initial(),
            0,
            ttl_ms,
        )
    }

    // ---------------------------------------------------------------
    // Lease validity window & accessors.
    // ---------------------------------------------------------------

    #[test]
    fn lease_window_is_half_open() {
        let orders = collection("orders");
        let lease = lease_for(&orders, "CN=node-a", 1_000);
        assert_eq!(lease.granted_at_ms(), 0);
        assert_eq!(lease.expires_at_ms(), 1_000);
        assert!(!lease.is_expired(0));
        assert!(!lease.is_expired(999));
        // The boundary instant is already expired — authority never extends to or
        // past the stated end.
        assert!(lease.is_expired(1_000));
        assert!(lease.is_expired(1_001));
        assert_eq!(lease.remaining_ms(250), 750);
        assert_eq!(lease.remaining_ms(1_000), 0);
        assert_eq!(lease.remaining_ms(5_000), 0);
    }

    #[test]
    fn lease_binds_term_range_owner_and_epoch() {
        let orders = collection("orders");
        let lease = lease_for(&orders, "CN=node-a", 1_000);
        assert_eq!(lease.supervisor_term(), SupervisorTerm::genesis());
        assert_eq!(lease.collection(), &orders);
        assert_eq!(lease.range_id(), RangeId::new(1));
        assert_eq!(lease.owner(), &ident("CN=node-a"));
        assert_eq!(lease.epoch(), OwnershipEpoch::initial());
    }

    // ---------------------------------------------------------------
    // evaluate(): the self-fence decision.
    // ---------------------------------------------------------------

    #[test]
    fn valid_lease_authorises_durable_writes() {
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        let mode = owner.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 500);
        assert_eq!(mode, OwnerWriteMode::Durable);
        assert!(mode.may_write_durable());
        assert!(!mode.is_fenced());
    }

    #[test]
    fn unleased_owner_is_fenced() {
        let owner = LeasedOwner::unleased();
        let mode = owner.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 0);
        assert_eq!(mode, OwnerWriteMode::Fenced(FenceReason::Unleased));
    }

    #[test]
    fn expired_lease_self_fences() {
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        // At t=1_500 the lease (window [0, 1_000)) has lapsed: the owner cannot
        // renew (Supervisor unreachable) so it self-fences.
        let mode = owner.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 1_500);
        match mode {
            OwnerWriteMode::Fenced(FenceReason::Expired {
                now_ms,
                expires_at_ms,
            }) => {
                assert_eq!(now_ms, 1_500);
                assert_eq!(expires_at_ms, 1_000);
            }
            other => panic!("expected Expired fence, got {other:?}"),
        }
    }

    #[test]
    fn epoch_mismatch_self_fences() {
        let orders = collection("orders");
        // Lease granted under epoch 1, but ownership has since moved to epoch 2.
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        let current_epoch = next_epoch();
        let mode = owner.evaluate(SupervisorTerm::genesis(), current_epoch, 500);
        match mode {
            OwnerWriteMode::Fenced(FenceReason::EpochSuperseded {
                lease_epoch,
                current_epoch: reported,
            }) => {
                assert_eq!(lease_epoch, OwnershipEpoch::initial());
                assert_eq!(reported, current_epoch);
            }
            other => panic!("expected EpochSuperseded fence, got {other:?}"),
        }
    }

    #[test]
    fn supervisor_term_advance_self_fences() {
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        // A new Supervisor leader bumped the term; the lease under the old term no
        // longer matches even though it has not expired.
        let current_term = SupervisorTerm::genesis().next();
        let mode = owner.evaluate(current_term, OwnershipEpoch::initial(), 500);
        match mode {
            OwnerWriteMode::Fenced(FenceReason::TermSuperseded {
                lease_term,
                current_term: reported,
            }) => {
                assert_eq!(lease_term, SupervisorTerm::genesis());
                assert_eq!(reported, current_term);
            }
            other => panic!("expected TermSuperseded fence, got {other:?}"),
        }
    }

    #[test]
    fn revoked_lease_self_fences_before_expiry() {
        let orders = collection("orders");
        let mut owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        owner.revoke();
        // Still inside the window and matching term/epoch, but explicitly revoked.
        let mode = owner.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 100);
        assert_eq!(mode, OwnerWriteMode::Fenced(FenceReason::Revoked));
    }

    #[test]
    fn revoke_takes_precedence_over_other_causes() {
        // Fail-closed ordering: an explicit revoke is reported even when the lease
        // is also expired and on a stale term/epoch.
        let orders = collection("orders");
        let mut owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        owner.revoke();
        let mode = owner.evaluate(SupervisorTerm::genesis().next(), next_epoch(), 10_000);
        assert_eq!(mode, OwnerWriteMode::Fenced(FenceReason::Revoked));
    }

    #[test]
    fn renewing_a_lease_clears_a_prior_revoke_and_extends_window() {
        let orders = collection("orders");
        let mut owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        owner.revoke();
        assert!(owner
            .evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 100)
            .is_fenced());
        // The Supervisor re-grants a fresh lease (e.g. a renewal at t=900 for
        // another 1_000 ms): authority is restored.
        owner.grant(OwnershipLease::grant(
            SupervisorTerm::genesis(),
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            900,
            1_000,
        ));
        let mode = owner.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 1_500);
        assert_eq!(mode, OwnerWriteMode::Durable);
    }

    // ---------------------------------------------------------------
    // admit_request(): self-fenced read mode.
    // ---------------------------------------------------------------

    #[test]
    fn valid_lease_admits_every_request_kind() {
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        for req in [
            RangeRequest::DurableWrite,
            RangeRequest::StaleRead,
            RangeRequest::ReplicationCatchUp,
        ] {
            assert!(owner
                .admit_request(
                    req,
                    SupervisorTerm::genesis(),
                    OwnershipEpoch::initial(),
                    500
                )
                .is_ok());
        }
    }

    #[test]
    fn self_fenced_read_mode_serves_reads_and_catch_up_but_rejects_durable_writes() {
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        // Past expiry: the owner is self-fenced.
        let now = 2_000;
        let term = SupervisorTerm::genesis();
        let epoch = OwnershipEpoch::initial();

        // Stale reads and replication catch-up are still served.
        assert!(owner
            .admit_request(RangeRequest::StaleRead, term, epoch, now)
            .is_ok());
        assert!(owner
            .admit_request(RangeRequest::ReplicationCatchUp, term, epoch, now)
            .is_ok());

        // Durable writes are rejected with the fence reason.
        let err = owner
            .admit_request(RangeRequest::DurableWrite, term, epoch, now)
            .unwrap_err();
        assert!(matches!(err.reason, FenceReason::Expired { .. }));
        assert!(err.to_string().contains("self-fenced"));
    }

    #[test]
    fn unleased_owner_rejects_durable_write_but_still_catches_up() {
        let owner = LeasedOwner::unleased();
        let term = SupervisorTerm::genesis();
        let epoch = OwnershipEpoch::initial();
        assert_eq!(
            owner
                .admit_request(RangeRequest::DurableWrite, term, epoch, 0)
                .unwrap_err()
                .reason,
            FenceReason::Unleased
        );
        // A brand-new member with no lease must still be allowed to catch up so it
        // can eventually become a valid owner.
        assert!(owner
            .admit_request(RangeRequest::ReplicationCatchUp, term, epoch, 0)
            .is_ok());
    }

    // ---------------------------------------------------------------
    // admit_durable_write(): lease in addition to catalog ownership.
    // ---------------------------------------------------------------

    #[test]
    fn durable_write_admitted_for_leased_owner() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        let range = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .expect("leased owner at current term/epoch may write");
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.range_id(), RangeId::new(1));
    }

    #[test]
    fn durable_write_rejected_for_catalog_owner_without_a_lease() {
        // node-a IS the catalog owner, but holds no lease — ownership alone is not
        // authority to write.
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let owner = LeasedOwner::unleased();
        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            0,
        )
        .unwrap_err();
        match err {
            DurableWriteReject::Fenced { reason, .. } => assert_eq!(reason, FenceReason::Unleased),
            other => panic!("expected Fenced(Unleased), got {other:?}"),
        }
    }

    #[test]
    fn durable_write_rejected_for_non_owner_before_lease_is_even_consulted() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // node-b is a replica. Even if it somehow held a lease, the catalog
        // ownership gate refuses it first.
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-b", 1_000));
        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-b"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .unwrap_err();
        match err {
            DurableWriteReject::NotOwner { role, owner, .. } => {
                assert_eq!(role, RangeRole::Replica);
                assert_eq!(owner, ident("CN=node-a"));
            }
            other => panic!("expected NotOwner, got {other:?}"),
        }
    }

    #[test]
    fn durable_write_rejected_when_no_range_covers_the_key() {
        let catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .unwrap_err();
        assert!(matches!(err, DurableWriteReject::NoRange { .. }));
    }

    #[test]
    fn durable_write_fenced_when_lease_epoch_trails_the_catalog() {
        // The catalog moves ownership a -> b -> a, so the live epoch is 3, but
        // node-a still holds its original epoch-1 lease. Catalog ownership matches
        // (node-a is owner again) yet the stale lease epoch fences the write.
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let stale_lease = lease_for(&orders, "CN=node-a", 100_000);

        let v1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        let v2 = v1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]);
        catalog.apply_update(v2.clone()).unwrap();
        let v3 = v2.transfer_to(ident("CN=node-a"), [ident("CN=node-b")]);
        catalog.apply_update(v3).unwrap();

        let owner = LeasedOwner::with_lease(stale_lease);
        let current_epoch = catalog.range(&orders, RangeId::new(1)).unwrap().epoch();
        assert_eq!(current_epoch.value(), 3);

        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .unwrap_err();
        match err {
            DurableWriteReject::Fenced {
                reason: FenceReason::EpochSuperseded { lease_epoch, .. },
                ..
            } => assert_eq!(lease_epoch, OwnershipEpoch::initial()),
            other => panic!("expected Fenced(EpochSuperseded), got {other:?}"),
        }
    }

    #[test]
    fn stale_owner_rejects_durable_write_after_epoch_bump() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let old_owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 100_000));

        let v1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        let v2 = v1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]);
        catalog.apply_update(v2).unwrap();
        let current = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(current.owner(), &ident("CN=node-b"));
        assert_eq!(current.epoch().value(), 2);

        let new_owner = LeasedOwner::with_lease(OwnershipLease::grant(
            SupervisorTerm::genesis(),
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-b"),
            current.epoch(),
            0,
            100_000,
        ));

        let err = admit_durable_write(
            &catalog,
            &old_owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .unwrap_err();
        match err {
            DurableWriteReject::StaleOwnership {
                attempted_owner,
                current_owner,
                attempted_epoch,
                current_epoch,
                ..
            } => {
                assert_eq!(attempted_owner, ident("CN=node-a"));
                assert_eq!(current_owner, ident("CN=node-b"));
                assert_eq!(attempted_epoch, OwnershipEpoch::initial());
                assert_eq!(current_epoch, current.epoch());
            }
            other => panic!("expected StaleOwnership, got {other:?}"),
        }

        let admitted = admit_durable_write(
            &catalog,
            &new_owner,
            &ident("CN=node-b"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .expect("new owner at the current epoch may write");
        assert_eq!(admitted.owner(), &ident("CN=node-b"));
        assert_eq!(admitted.epoch(), current.epoch());
    }

    #[test]
    fn durable_write_fenced_when_lease_is_for_a_different_range() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // node-a is the owner of range 1, but its lease names range 2.
        let wrong_range_lease = OwnershipLease::grant(
            SupervisorTerm::genesis(),
            orders.clone(),
            RangeId::new(2),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            0,
            1_000,
        );
        let owner = LeasedOwner::with_lease(wrong_range_lease);
        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            SupervisorTerm::genesis(),
            500,
        )
        .unwrap_err();
        // A lease that does not cover this range is no authority for it.
        match err {
            DurableWriteReject::Fenced { reason, .. } => assert_eq!(reason, FenceReason::Unleased),
            other => panic!("expected Fenced(Unleased), got {other:?}"),
        }
    }

    #[test]
    fn durable_write_rejected_after_self_fence_then_restored_on_renewal() {
        // End-to-end: a leased owner writes, its lease lapses (fenced), and a
        // renewal restores durable writes — the lease, not catalog ownership, is
        // the thing that gates here.
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let mut owner = LeasedOwner::with_lease(lease_for(&orders, "CN=node-a", 1_000));
        let term = SupervisorTerm::genesis();

        // t=500: valid.
        assert!(admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            term,
            500
        )
        .is_ok());
        // t=2_000: lapsed -> fenced.
        let err = admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            term,
            2_000,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DurableWriteReject::Fenced {
                reason: FenceReason::Expired { .. },
                ..
            }
        ));
        // Renew under the same term/epoch from t=2_000: durable writes resume.
        owner.grant(OwnershipLease::grant(
            term,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            2_000,
            1_000,
        ));
        assert!(admit_durable_write(
            &catalog,
            &owner,
            &ident("CN=node-a"),
            &orders,
            b"k",
            term,
            2_500
        )
        .is_ok());
    }
}
