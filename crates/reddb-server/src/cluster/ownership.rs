//! The global shard ownership catalog (issue #989, PRD #987, ADR 0037).
//!
//! The shard ownership catalog is the **source of truth for range routing and
//! failover** in a multi-writer cluster. Per the glossary it is *"explicit,
//! versioned RedDB catalog state that records shard/range bounds, current writer
//! owner, replicas, and ownership epoch/version"* and — crucially — it is
//! *"special global control-plane state replicated to all data members rather
//! than sharded like ordinary user collections"*.
//!
//! That last point shapes the whole module. The catalog is the thing that tells
//! you *where* a collection's data lives, so it cannot itself be located by the
//! same user-data sharding it describes — that would be circular. Instead every
//! data member holds a full [`ShardOwnershipCatalog`] replica and routes against
//! it locally ([`ShardOwnershipCatalog::route`]). Replication is modelled as
//! shipping versioned [`RangeOwnership`] entries that each member applies through
//! the same [`apply_update`](ShardOwnershipCatalog::apply_update) path that the
//! Supervisor leader writes through — and that path rejects stale versions, so a
//! late or out-of-order replica update can never overwrite newer ownership.
//!
//! ## What the catalog records
//!
//! One [`RangeOwnership`] entry per owned shard/range carries everything routing
//! and fencing need:
//!
//! * [`CollectionId`] + [`RangeId`] — *which* range of *which* collection.
//! * [`ShardKeyMode`] — [`Hash`](ShardKeyMode::Hash) (the default, for uniform
//!   distribution) or [`Ordered`](ShardKeyMode::Ordered) (declared when range
//!   locality and ordered scans matter more than hotspot resistance).
//! * [`RangeBounds`] — the half-open `[lower, upper)` partition this entry owns.
//! * `owner` ([`NodeIdentity`]) + `replicas` — the current single writer for the
//!   range and its read/catch-up copies.
//! * [`OwnershipEpoch`] + [`CatalogVersion`] — the fencing epoch (bumped on owner
//!   change so a stale old owner is fenced) and the monotonic write version
//!   (bumped on *every* accepted update so stale writes are rejected).
//! * [`PlacementMetadata`] — replication factor and free-form placement
//!   attributes (region/zone/weight) the rebalancer reads.
//!
//! Ownership changes are produced as *transitions* — new entries built with
//! [`RangeOwnership::transfer_to`] / [`update_replicas`](RangeOwnership::update_replicas)
//! / [`update_placement`](RangeOwnership::update_placement) — never arbitrary row
//! edits, matching ADR 0037's "transitions, not arbitrary row edits".
//!
//! Everything here is a pure data model with no I/O, so the routing, versioning,
//! and replication story is exercised deterministically.

use std::collections::BTreeMap;

use super::identity::NodeIdentity;
use super::slot::hash_shard_key_to_range_key;

/// A user collection's stable identity, as recorded in the catalog.
///
/// The catalog is keyed by collection (and range within it); this is the
/// collection's own name, not a shard-routed handle. Resolving a
/// [`CollectionId`] to its ranges needs only the catalog itself — no user-data
/// sharding — which is what lets the catalog be the thing that *bootstraps*
/// routing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionIdError;

impl std::fmt::Display for CollectionIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "collection id is empty")
    }
}

impl std::error::Error for CollectionIdError {}

impl CollectionId {
    /// Build a collection id from a non-empty name.
    pub fn new(value: impl AsRef<str>) -> Result<Self, CollectionIdError> {
        let value = value.as_ref().trim();
        if value.is_empty() {
            return Err(CollectionIdError);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CollectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identifier for one shard/range within a collection.
///
/// Ranges are owned at sub-collection granularity (ADR 0037), so a collection
/// can have many of these — each is one independently-owned, independently-routed
/// partition. The id is stable across ownership transitions: moving a range to a
/// new owner keeps its [`RangeId`] and bumps its epoch/version, it does not mint
/// a new range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RangeId(u64);

impl RangeId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Collection-level partitioning mode for shard/range ownership.
///
/// Per the glossary, *"Hash mode is the default for uniform distribution;
/// ordered mode is declared when range locality and ordered scans matter more
/// than automatic hotspot resistance."* The mode is fixed per collection: every
/// range of a collection shares its mode, and an entry whose mode disagrees with
/// its collection's declared mode is rejected
/// ([`CatalogError::ShardKeyModeMismatch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShardKeyMode {
    /// Uniform hash distribution — the default. Bounds are over hash tokens.
    #[default]
    Hash,
    /// Ordered key ranges, declared for range locality / ordered scans. Bounds
    /// are over the ordered shard key itself.
    Ordered,
}

/// One edge of a [`RangeBounds`].
///
/// Bounds are byte strings so the same type serves both shard key modes: a
/// [`Hash`](ShardKeyMode::Hash) range bounds hash-token bytes, an
/// [`Ordered`](ShardKeyMode::Ordered) range bounds the ordered key bytes
/// directly. [`Min`](RangeBound::Min)/[`Max`](RangeBound::Max) are the open ends
/// of the keyspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeBound {
    /// The open low end of the keyspace (everything is `>= Min`).
    Min,
    /// A concrete boundary key.
    Key(Vec<u8>),
    /// The open high end of the keyspace (everything is `< Max`).
    Max,
}

impl RangeBound {
    /// A boundary at the given key bytes.
    pub fn key(bytes: impl Into<Vec<u8>>) -> Self {
        RangeBound::Key(bytes.into())
    }

    /// Total order over keyspace positions: `Min < every Key < Max`, with keys
    /// compared lexicographically. This is what makes both `contains` and
    /// `overlaps` plain comparisons.
    fn position(&self) -> Position<'_> {
        match self {
            RangeBound::Min => Position::Min,
            RangeBound::Key(k) => Position::Key(k),
            RangeBound::Max => Position::Max,
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Position<'a> {
    Min,
    Key(&'a [u8]),
    Max,
}

/// The half-open `[lower, upper)` partition a range owns.
///
/// Half-open bounds tile the keyspace without gaps or double-cover: adjacent
/// ranges share a boundary key that belongs to exactly one of them. `lower` must
/// be strictly below `upper`, so an empty or inverted range cannot be
/// constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeBounds {
    lower: RangeBound,
    upper: RangeBound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeBoundsError;

impl std::fmt::Display for RangeBoundsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "range lower bound must be strictly below the upper bound"
        )
    }
}

impl std::error::Error for RangeBoundsError {}

impl RangeBounds {
    /// Bounds from an explicit `[lower, upper)` pair. Errors if the range would
    /// be empty or inverted (`lower >= upper`).
    pub fn new(lower: RangeBound, upper: RangeBound) -> Result<Self, RangeBoundsError> {
        if lower.position() >= upper.position() {
            return Err(RangeBoundsError);
        }
        Ok(Self { lower, upper })
    }

    /// The whole keyspace, `[Min, Max)` — a single range covering a collection.
    pub fn full() -> Self {
        Self {
            lower: RangeBound::Min,
            upper: RangeBound::Max,
        }
    }

    pub fn lower(&self) -> &RangeBound {
        &self.lower
    }

    pub fn upper(&self) -> &RangeBound {
        &self.upper
    }

    /// Does `key` fall inside this half-open range? Lower bound inclusive, upper
    /// bound exclusive — the routing predicate.
    pub fn contains(&self, key: &[u8]) -> bool {
        let key = Position::Key(key);
        self.lower.position() <= key && key < self.upper.position()
    }

    /// Do these two ranges share any key? Used to keep a collection's ranges
    /// non-overlapping so routing resolves to exactly one owner.
    pub fn overlaps(&self, other: &RangeBounds) -> bool {
        self.lower.position() < other.upper.position()
            && other.lower.position() < self.upper.position()
    }

    /// Split this `[lower, upper)` range at `at` into a lower child
    /// `[lower, at)` and an upper child `[at, upper)`. The split point must fall
    /// **strictly inside** the range (`lower < at < upper`); a point at or
    /// outside a bound would carve off an empty child and is rejected with
    /// [`RangeBoundsError`]. The two children tile the original exactly — no gap,
    /// no overlap — which is what lets a [split-and-move](super::move_range) shrink
    /// the retained child and create the moved child without making routing
    /// ambiguous.
    pub fn split_at(&self, at: &[u8]) -> Result<(RangeBounds, RangeBounds), RangeBoundsError> {
        let at_pos = Position::Key(at);
        if at_pos <= self.lower.position() || at_pos >= self.upper.position() {
            return Err(RangeBoundsError);
        }
        let lower = RangeBounds {
            lower: self.lower.clone(),
            upper: RangeBound::key(at.to_vec()),
        };
        let upper = RangeBounds {
            lower: RangeBound::key(at.to_vec()),
            upper: self.upper.clone(),
        };
        Ok((lower, upper))
    }
}

/// Monotonic write version of a single catalog entry.
///
/// Every accepted update to a range bumps its version. An update that does not
/// strictly advance the version is stale and is rejected — this is the
/// compare-and-advance rule that makes catalog replication safe regardless of
/// delivery order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogVersion(u64);

impl CatalogVersion {
    /// The version a range is created at.
    pub fn initial() -> Self {
        Self(1)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for CatalogVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Fencing epoch for a range's write authority.
///
/// Distinct from [`CatalogVersion`]: the version advances on *any* catalog edit,
/// but the epoch advances only when **write authority moves** (a new owner). A
/// WAL/logical record stamped with an epoch older than the catalog's current
/// epoch is from a fenced old owner and must be rejected (ADR 0037, "fencing is
/// enforced below routing").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnershipEpoch(u64);

impl OwnershipEpoch {
    /// The epoch a range is created at.
    pub fn initial() -> Self {
        Self(1)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for OwnershipEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Placement metadata the rebalancer reads when planning transitions.
///
/// The MVP carries the range's replication factor plus a free-form attribute map
/// for region/zone/operator-weight hints. It is descriptive control-plane data,
/// not an authorization source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementMetadata {
    replication_factor: usize,
    attributes: BTreeMap<String, String>,
}

impl PlacementMetadata {
    /// Placement with a target replication factor and no attributes.
    pub fn with_replication_factor(replication_factor: usize) -> Self {
        Self {
            replication_factor,
            attributes: BTreeMap::new(),
        }
    }

    /// Attach a placement attribute (e.g. `region` → `us-east-1`).
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn replication_factor(&self) -> usize {
        self.replication_factor
    }

    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }
}

/// One owned shard/range: the catalog's unit of routing and fencing.
///
/// An entry is self-describing — it carries its collection, range id, mode,
/// bounds, owner, replicas, epoch, version, and placement — so a data member
/// that receives it by replication can route and fence without consulting any
/// other state. New ownership states are produced as *transitions* off an
/// existing entry ([`transfer_to`](Self::transfer_to),
/// [`update_replicas`](Self::update_replicas),
/// [`update_placement`](Self::update_placement)), each of which advances the
/// version — and, for an owner change, the fencing epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeOwnership {
    collection: CollectionId,
    range_id: RangeId,
    shard_key_mode: ShardKeyMode,
    bounds: RangeBounds,
    owner: NodeIdentity,
    replicas: Vec<NodeIdentity>,
    epoch: OwnershipEpoch,
    version: CatalogVersion,
    placement: PlacementMetadata,
}

impl RangeOwnership {
    /// The initial ownership state for a freshly created range: version and
    /// epoch both at their [`initial`](CatalogVersion::initial) values.
    #[allow(clippy::too_many_arguments)]
    pub fn establish(
        collection: CollectionId,
        range_id: RangeId,
        shard_key_mode: ShardKeyMode,
        bounds: RangeBounds,
        owner: NodeIdentity,
        replicas: impl IntoIterator<Item = NodeIdentity>,
        placement: PlacementMetadata,
    ) -> Self {
        Self {
            collection,
            range_id,
            shard_key_mode,
            bounds,
            owner,
            replicas: replicas.into_iter().collect(),
            epoch: OwnershipEpoch::initial(),
            version: CatalogVersion::initial(),
            placement,
        }
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    pub fn shard_key_mode(&self) -> ShardKeyMode {
        self.shard_key_mode
    }

    pub fn bounds(&self) -> &RangeBounds {
        &self.bounds
    }

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn replicas(&self) -> &[NodeIdentity] {
        &self.replicas
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }

    pub fn version(&self) -> CatalogVersion {
        self.version
    }

    pub fn placement(&self) -> &PlacementMetadata {
        &self.placement
    }

    /// The catalog key for this entry: `(collection, range_id)`.
    fn key(&self) -> (CollectionId, RangeId) {
        (self.collection.clone(), self.range_id)
    }

    /// A transition that moves write authority to `new_owner` with `new_replicas`.
    /// Advances **both** the version (it is a catalog write) and the ownership
    /// epoch (write authority moved, so any old owner is fenced).
    pub fn transfer_to(
        &self,
        new_owner: NodeIdentity,
        new_replicas: impl IntoIterator<Item = NodeIdentity>,
    ) -> Self {
        Self {
            owner: new_owner,
            replicas: new_replicas.into_iter().collect(),
            epoch: self.epoch.next(),
            version: self.version.next(),
            ..self.clone()
        }
    }

    /// A transition that changes only the replica set. Advances the version but
    /// **not** the epoch: write authority did not move, so no owner is fenced.
    pub fn update_replicas(&self, new_replicas: impl IntoIterator<Item = NodeIdentity>) -> Self {
        Self {
            replicas: new_replicas.into_iter().collect(),
            version: self.version.next(),
            ..self.clone()
        }
    }

    /// A transition that changes only placement metadata. Advances the version
    /// but not the epoch.
    pub fn update_placement(&self, placement: PlacementMetadata) -> Self {
        Self {
            placement,
            version: self.version.next(),
            ..self.clone()
        }
    }

    /// A transition that **re-bounds** the range without moving write authority —
    /// the retained-child step of a range split, which narrows this entry to the
    /// keys its owner keeps while a sibling entry takes the carved-off subrange.
    /// Advances the version but **not** the epoch: the same owner keeps writing
    /// the retained keys, so no one is fenced.
    pub fn with_bounds(&self, bounds: RangeBounds) -> Self {
        Self {
            bounds,
            version: self.version.next(),
            ..self.clone()
        }
    }

    /// This node's [`RangeRole`] for *this* range (issue #990).
    ///
    /// A data member is the single writer ([`Owner`](RangeRole::Owner)) of a
    /// range, holds a read/catch-up copy ([`Replica`](RangeRole::Replica)), or
    /// holds no copy at all ([`NoCopy`](RangeRole::NoCopy)). The role is
    /// per-range, not a global node role: the same node can be owner of one
    /// range, replica of another, and uninvolved in a third — which is why this
    /// is the input to the ownership-aware public-write gate rather than the
    /// instance-wide [`WriteGate`](crate::runtime::write_gate::WriteGate).
    pub fn role_of(&self, node: &NodeIdentity) -> RangeRole {
        if self.owner == *node {
            RangeRole::Owner
        } else if self.replicas.iter().any(|replica| replica == node) {
            RangeRole::Replica
        } else {
            RangeRole::NoCopy
        }
    }
}

/// A data member's role for one specific range (issue #990, PRD #987).
///
/// Distinguishes the three positions a node can hold relative to a range, which
/// the ownership-aware write gate
/// ([`admit_public_write`](ShardOwnershipCatalog::admit_public_write)) turns
/// into an allow/reject decision: only the current [`Owner`](Self::Owner) may
/// take a *public* write for the range; a [`Replica`](Self::Replica) and a
/// [`NoCopy`](Self::NoCopy) node both reject it and the caller must route to the
/// owner. (A replica still applies the owner's changes through the privileged
/// internal apply path — that path is gated by the range-authority fence from
/// issue #991, not by this public gate.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRole {
    /// The current single writer for the range — the only role a public write
    /// may land on.
    Owner,
    /// Holds a read/catch-up copy but is not the writer. Public writes are
    /// rejected and routed to the owner; replicated changes still flow in via
    /// the privileged internal apply path.
    Replica,
    /// Holds no copy of the range at all.
    NoCopy,
}

impl RangeRole {
    /// Whether this role may accept a *public* write for the range. Only the
    /// owner may; replica and no-copy may not.
    pub fn may_write_public(self) -> bool {
        matches!(self, RangeRole::Owner)
    }

    fn label(self) -> &'static str {
        match self {
            RangeRole::Owner => "owner",
            RangeRole::Replica => "replica",
            RangeRole::NoCopy => "no-copy",
        }
    }
}

/// Whether an accepted update created a new range or advanced an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The range did not exist and was created at this version.
    Created,
    /// An existing range advanced to a newer version.
    Updated,
}

/// Why a catalog update was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The update's version did not strictly advance the range's current
    /// version — a stale or out-of-order write. Carries both versions so the
    /// caller (or a replica) can see how far behind it was.
    StaleVersion {
        collection: CollectionId,
        range_id: RangeId,
        current: CatalogVersion,
        attempted: CatalogVersion,
    },
    /// The entry's shard key mode disagrees with the collection's declared mode.
    /// A collection is hash- *or* ordered-partitioned, never both.
    ShardKeyModeMismatch {
        collection: CollectionId,
        declared: ShardKeyMode,
        attempted: ShardKeyMode,
    },
    /// Creating this range would overlap an existing range of the same
    /// collection, which would make routing ambiguous.
    OverlappingRange {
        collection: CollectionId,
        existing: RangeId,
        attempted: RangeId,
    },
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleVersion {
                collection,
                range_id,
                current,
                attempted,
            } => write!(
                f,
                "stale catalog update for {collection}/{range_id}: current version {current}, attempted {attempted}"
            ),
            Self::ShardKeyModeMismatch {
                collection,
                declared,
                attempted,
            } => write!(
                f,
                "collection {collection} is declared {declared:?} but range uses {attempted:?}"
            ),
            Self::OverlappingRange {
                collection,
                existing,
                attempted,
            } => write!(
                f,
                "range {attempted} overlaps existing range {existing} of collection {collection}"
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

/// Why an ownership-aware *public* write was rejected (issue #990).
///
/// This is a **routing/ownership** error, deliberately distinct from the
/// instance-wide read-only rejection raised by
/// [`WriteGate`](crate::runtime::write_gate::WriteGate): a replica/non-holder
/// rejecting a public write is not "this node is read-only", it is "this node is
/// not the authority for *this range* — route to the owner". Crucially, none of
/// these rejections fall back to the privileged internal replica-apply path; a
/// public write that is not for this node's owned range never reaches storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeWriteReject {
    /// No range of the collection covers the routed key, so the write cannot be
    /// placed. The caller must (re)resolve routing against a fresher catalog.
    NoRange { collection: CollectionId },
    /// This node holds the range but is not its owner (a [`Replica`]), or holds
    /// no copy at all ([`NoCopy`]). Either way a public write must be routed to
    /// `owner`, never applied locally.
    ///
    /// [`Replica`]: RangeRole::Replica
    /// [`NoCopy`]: RangeRole::NoCopy
    NotOwner {
        collection: CollectionId,
        range_id: RangeId,
        role: RangeRole,
        owner: NodeIdentity,
    },
    /// This node *is* the range owner, but the write was authorised under an
    /// ownership epoch that no longer matches the catalog — a write fenced out
    /// because ownership has since moved (its epoch advanced). Carries both
    /// epochs so the caller can see how far the routing decision was behind.
    StaleEpoch {
        collection: CollectionId,
        range_id: RangeId,
        expected: OwnershipEpoch,
        current: OwnershipEpoch,
    },
}

impl std::fmt::Display for RangeWriteReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRange { collection } => write!(
                f,
                "no range of collection {collection} covers the routed key — re-resolve routing"
            ),
            Self::NotOwner {
                collection,
                range_id,
                role,
                owner,
            } => write!(
                f,
                "this node is {} of {collection}/{range_id}, not its owner — route the write to {owner}",
                role.label()
            ),
            Self::StaleEpoch {
                collection,
                range_id,
                expected,
                current,
            } => write!(
                f,
                "stale ownership epoch for {collection}/{range_id}: write authorised under epoch {expected}, current is {current}"
            ),
        }
    }
}

impl std::error::Error for RangeWriteReject {}

/// The global shard ownership catalog held by every data member.
///
/// This single type plays both roles in ADR 0037's model: it is the authoritative
/// state the Cluster Supervisor leader writes through, and it is the replica each
/// data member holds and routes against. Both write through
/// [`apply_update`](Self::apply_update), so the stale-version rejection that
/// makes leader writes versioned is the *same* rule that makes replica
/// application order-independent. Nothing here needs user-data sharding to find
/// an entry: ranges are addressed directly by `(collection, range_id)`, and
/// routing ([`route`](Self::route)) is a local scan of the replica.
#[derive(Debug, Clone, Default)]
pub struct ShardOwnershipCatalog {
    /// Declared shard key mode per collection. A collection is recorded here the
    /// moment its first range is created (or via [`declare_collection`]).
    ///
    /// [`declare_collection`]: Self::declare_collection
    collections: BTreeMap<CollectionId, ShardKeyMode>,
    ranges: BTreeMap<(CollectionId, RangeId), RangeOwnership>,
}

impl ShardOwnershipCatalog {
    /// An empty catalog — a cluster with no collections placed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a collection's shard key mode up front. Hash is the default, so
    /// this is mainly how an operator opts a collection into
    /// [`Ordered`](ShardKeyMode::Ordered) mode before any range exists. Declaring
    /// the same mode twice is idempotent; redeclaring a different mode for a
    /// collection that already has a mode is a [`ShardKeyModeMismatch`].
    ///
    /// [`ShardKeyModeMismatch`]: CatalogError::ShardKeyModeMismatch
    pub fn declare_collection(
        &mut self,
        collection: CollectionId,
        mode: ShardKeyMode,
    ) -> Result<(), CatalogError> {
        match self.collections.get(&collection) {
            Some(&declared) if declared != mode => Err(CatalogError::ShardKeyModeMismatch {
                collection,
                declared,
                attempted: mode,
            }),
            _ => {
                self.collections.insert(collection, mode);
                Ok(())
            }
        }
    }

    /// The declared shard key mode of `collection`, if it has any ranges or was
    /// explicitly declared.
    pub fn shard_key_mode(&self, collection: &CollectionId) -> Option<ShardKeyMode> {
        self.collections.get(collection).copied()
    }

    /// Apply a versioned ownership update — the single write path for both leader
    /// writes and replica application.
    ///
    /// Creation (the range does not yet exist) auto-declares the collection's
    /// mode from the entry and checks the new range does not overlap a sibling.
    /// Updating an existing range requires the entry's version to **strictly
    /// advance** the current version; anything else is a
    /// [`StaleVersion`](CatalogError::StaleVersion) rejection that leaves the
    /// catalog untouched. Either way the entry's mode must match the collection's
    /// declared mode.
    pub fn apply_update(&mut self, entry: RangeOwnership) -> Result<UpdateOutcome, CatalogError> {
        // Mode must agree with the collection (auto-declared on first range).
        match self.collections.get(entry.collection()) {
            Some(&declared) if declared != entry.shard_key_mode() => {
                return Err(CatalogError::ShardKeyModeMismatch {
                    collection: entry.collection().clone(),
                    declared,
                    attempted: entry.shard_key_mode(),
                });
            }
            _ => {}
        }

        let key = entry.key();
        match self.ranges.get(&key) {
            Some(current) => {
                if entry.version() <= current.version() {
                    return Err(CatalogError::StaleVersion {
                        collection: entry.collection().clone(),
                        range_id: entry.range_id(),
                        current: current.version(),
                        attempted: entry.version(),
                    });
                }
                if let Some(existing) = self.overlapping_sibling(&entry) {
                    return Err(CatalogError::OverlappingRange {
                        collection: entry.collection().clone(),
                        existing,
                        attempted: entry.range_id(),
                    });
                }
                self.collections
                    .insert(entry.collection().clone(), entry.shard_key_mode());
                self.ranges.insert(key, entry);
                Ok(UpdateOutcome::Updated)
            }
            None => {
                // Creating a range: it must not overlap any sibling range of the
                // same collection, or routing would be ambiguous.
                if let Some(existing) = self.overlapping_sibling(&entry) {
                    return Err(CatalogError::OverlappingRange {
                        collection: entry.collection().clone(),
                        existing,
                        attempted: entry.range_id(),
                    });
                }
                self.collections
                    .insert(entry.collection().clone(), entry.shard_key_mode());
                self.ranges.insert(key, entry);
                Ok(UpdateOutcome::Created)
            }
        }
    }

    fn overlapping_sibling(&self, entry: &RangeOwnership) -> Option<RangeId> {
        self.ranges_for(entry.collection())
            .find(|range| {
                range.range_id() != entry.range_id() && range.bounds().overlaps(entry.bounds())
            })
            .map(RangeOwnership::range_id)
    }

    /// The current ownership of one range, addressed directly by identity — no
    /// routing required, because the catalog is what routing is built on.
    pub fn range(&self, collection: &CollectionId, range_id: RangeId) -> Option<&RangeOwnership> {
        self.ranges.get(&(collection.clone(), range_id))
    }

    /// Every range of `collection`, in range-id order.
    pub fn ranges_for<'a>(
        &'a self,
        collection: &CollectionId,
    ) -> impl Iterator<Item = &'a RangeOwnership> {
        let collection = collection.clone();
        self.ranges
            .iter()
            .filter(move |((c, _), _)| *c == collection)
            .map(|(_, r)| r)
    }

    /// Route a normalized range key to the range that owns it — the catalog read
    /// every routing decision makes. Returns the owning [`RangeOwnership`] (whose
    /// `owner`, `epoch`, and `replicas` the caller uses to send and fence the
    /// write), or `None` if no range covers the key yet.
    pub fn route(&self, collection: &CollectionId, key: &[u8]) -> Option<&RangeOwnership> {
        self.ranges_for(collection)
            .find(|r| r.bounds().contains(key))
    }

    /// Route a logical shard key according to the collection's declared mode.
    ///
    /// Ordered collections route the shard key bytes directly. Hash collections
    /// first map the shard key into a stable hash slot and route that slot's
    /// range key through the same `collection -> range` catalog.
    pub fn route_shard_key(
        &self,
        collection: &CollectionId,
        shard_key: &[u8],
    ) -> Option<&RangeOwnership> {
        match self.shard_key_mode(collection)? {
            ShardKeyMode::Ordered => self.route(collection, shard_key),
            ShardKeyMode::Hash => {
                let range_key = hash_shard_key_to_range_key(shard_key);
                self.route(collection, &range_key)
            }
        }
    }

    /// This node's [`RangeRole`] for a directly-addressed range (issue #990).
    /// Returns `None` when no such range exists in the catalog — distinct from
    /// [`NoCopy`](RangeRole::NoCopy), which means the range exists but this node
    /// holds no copy of it.
    pub fn role_at(
        &self,
        node: &NodeIdentity,
        collection: &CollectionId,
        range_id: RangeId,
    ) -> Option<RangeRole> {
        self.range(collection, range_id)
            .map(|range| range.role_of(node))
    }

    /// Ownership-aware gate for a **public** write (issue #990, PRD #987).
    ///
    /// Routes `key` to its range, then admits the write only when `node` is the
    /// range's current [`Owner`](RangeRole::Owner) **and** `expected_epoch`
    /// matches the range's current ownership epoch. On success returns the owned
    /// [`RangeOwnership`] (so the caller can proceed with the write against the
    /// authoritative epoch); otherwise a [`RangeWriteReject`] explaining why.
    ///
    /// This is the public surface's gate — the counterpart of the instance-wide
    /// [`WriteGate`](crate::runtime::write_gate::WriteGate) for multi-writer,
    /// per-range ownership. The internal replica-apply path does **not** consult
    /// it: replicated changes flow into a replica through the privileged apply
    /// path (fenced by issue #991's range-authority watermark), so a node that
    /// rejects a *public* write here can still legitimately apply the owner's
    /// replicated changes for the very same range.
    pub fn admit_public_write(
        &self,
        node: &NodeIdentity,
        collection: &CollectionId,
        key: &[u8],
        expected_epoch: OwnershipEpoch,
    ) -> Result<&RangeOwnership, RangeWriteReject> {
        let range =
            self.route_shard_key(collection, key)
                .ok_or_else(|| RangeWriteReject::NoRange {
                    collection: collection.clone(),
                })?;
        let role = range.role_of(node);
        if !role.may_write_public() {
            return Err(RangeWriteReject::NotOwner {
                collection: collection.clone(),
                range_id: range.range_id(),
                role,
                owner: range.owner().clone(),
            });
        }
        if expected_epoch != range.epoch() {
            return Err(RangeWriteReject::StaleEpoch {
                collection: collection.clone(),
                range_id: range.range_id(),
                expected: expected_epoch,
                current: range.epoch(),
            });
        }
        Ok(range)
    }

    /// Total number of owned ranges across all collections.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// All ranges, in `(collection, range_id)` order — the full catalog content
    /// a joining member adopts as its starting replica
    /// (see [`ControlPlaneSnapshot`](super::join::ControlPlaneSnapshot)).
    pub fn entries(&self) -> impl Iterator<Item = &RangeOwnership> {
        self.ranges.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn bounds(lower: &[u8], upper: &[u8]) -> RangeBounds {
        RangeBounds::new(RangeBound::key(lower), RangeBound::key(upper)).unwrap()
    }

    /// A hash range over `[lower, Max)` owned by `owner`.
    fn hash_range(coll: &CollectionId, id: u64, bnds: RangeBounds, owner: &str) -> RangeOwnership {
        RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Hash,
            bnds,
            ident(owner),
            [ident("CN=replica-1")],
            PlacementMetadata::with_replication_factor(3),
        )
    }

    fn single_hash_slot_bounds(key: &[u8]) -> RangeBounds {
        let slot = super::super::slot::hash_shard_key_to_slot(key);
        let lower = RangeBound::key(slot.range_key());
        let upper = match slot.value().checked_add(1) {
            Some(next) if next < super::super::slot::PRODUCTION_HASH_SLOT_COUNT => {
                RangeBound::key(super::super::slot::HashSlot::new(next).unwrap().range_key())
            }
            _ => RangeBound::Max,
        };
        RangeBounds::new(lower, upper).unwrap()
    }

    #[test]
    fn empty_catalog_creation() {
        let catalog = ShardOwnershipCatalog::new();
        assert_eq!(catalog.range_count(), 0);
        assert!(catalog.shard_key_mode(&collection("orders")).is_none());
    }

    #[test]
    fn hash_is_the_default_shard_key_mode() {
        // The first range of a collection auto-declares its mode; a range built
        // with the default mode lands the collection in Hash mode.
        assert_eq!(ShardKeyMode::default(), ShardKeyMode::Hash);

        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(hash_range(&orders, 1, RangeBounds::full(), "CN=node-a"))
            .unwrap();
        assert_eq!(catalog.shard_key_mode(&orders), Some(ShardKeyMode::Hash));
    }

    #[test]
    fn hash_range_entry_routes_to_owner() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");

        // Two hash token ranges split at 0x80.
        catalog
            .apply_update(hash_range(
                &orders,
                1,
                RangeBounds::new(RangeBound::Min, RangeBound::key([0x80])).unwrap(),
                "CN=node-a",
            ))
            .unwrap();
        catalog
            .apply_update(hash_range(
                &orders,
                2,
                RangeBounds::new(RangeBound::key([0x80]), RangeBound::Max).unwrap(),
                "CN=node-b",
            ))
            .unwrap();

        // Routing reads expose the owner for a key without any user-data sharding.
        assert_eq!(
            catalog.route(&orders, &[0x10]).unwrap().owner(),
            &ident("CN=node-a")
        );
        assert_eq!(
            catalog.route(&orders, &[0x80]).unwrap().owner(),
            &ident("CN=node-b")
        );
        assert_eq!(
            catalog.route(&orders, &[0xff]).unwrap().owner(),
            &ident("CN=node-b")
        );
        // The routing read also exposes replicas and fencing epoch.
        let r = catalog.route(&orders, &[0x10]).unwrap();
        assert_eq!(r.replicas(), &[ident("CN=replica-1")]);
        assert_eq!(r.epoch(), OwnershipEpoch::initial());
    }

    #[test]
    fn hash_mode_routes_logical_shard_key_through_hash_slot() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let key = b"tenant:42";
        catalog
            .apply_update(hash_range(
                &orders,
                1,
                single_hash_slot_bounds(key),
                "CN=node-a",
            ))
            .unwrap();

        let routed = catalog
            .route_shard_key(&orders, key)
            .expect("hash slot range covers the logical shard key");
        assert_eq!(routed.owner(), &ident("CN=node-a"));
    }

    #[test]
    fn ordered_mode_can_be_declared_and_routed() {
        let mut catalog = ShardOwnershipCatalog::new();
        let events = collection("events");
        catalog
            .declare_collection(events.clone(), ShardKeyMode::Ordered)
            .unwrap();
        assert_eq!(catalog.shard_key_mode(&events), Some(ShardKeyMode::Ordered));

        // Ordered ranges bound the ordered key itself: [a, m) and [m, z).
        catalog
            .apply_update(RangeOwnership::establish(
                events.clone(),
                RangeId::new(1),
                ShardKeyMode::Ordered,
                bounds(b"a", b"m"),
                ident("CN=node-a"),
                [],
                PlacementMetadata::with_replication_factor(3),
            ))
            .unwrap();
        catalog
            .apply_update(RangeOwnership::establish(
                events.clone(),
                RangeId::new(2),
                ShardKeyMode::Ordered,
                bounds(b"m", b"z"),
                ident("CN=node-b"),
                [],
                PlacementMetadata::with_replication_factor(3),
            ))
            .unwrap();

        assert_eq!(
            catalog.route(&events, b"alpha").unwrap().owner(),
            &ident("CN=node-a")
        );
        assert_eq!(
            catalog.route(&events, b"mike").unwrap().owner(),
            &ident("CN=node-b")
        );
        // A key outside every declared range routes nowhere.
        assert!(catalog.route(&events, b"zzz").is_none());
    }

    #[test]
    fn declaring_a_conflicting_mode_is_rejected() {
        let mut catalog = ShardOwnershipCatalog::new();
        let events = collection("events");
        catalog
            .declare_collection(events.clone(), ShardKeyMode::Ordered)
            .unwrap();
        // Redeclaring the same mode is fine.
        catalog
            .declare_collection(events.clone(), ShardKeyMode::Ordered)
            .unwrap();
        // A different mode is a mismatch.
        let err = catalog
            .declare_collection(events.clone(), ShardKeyMode::Hash)
            .unwrap_err();
        assert_eq!(
            err,
            CatalogError::ShardKeyModeMismatch {
                collection: events.clone(),
                declared: ShardKeyMode::Ordered,
                attempted: ShardKeyMode::Hash,
            }
        );
        // And a range whose mode disagrees with the declared collection is rejected.
        let err = catalog
            .apply_update(hash_range(&events, 1, RangeBounds::full(), "CN=node-a"))
            .unwrap_err();
        assert!(matches!(err, CatalogError::ShardKeyModeMismatch { .. }));
    }

    #[test]
    fn version_bumps_on_owner_transfer_and_epoch_fences() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(hash_range(&orders, 1, RangeBounds::full(), "CN=node-a"))
            .unwrap();

        let current = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(current.version(), CatalogVersion::initial());
        assert_eq!(current.epoch(), OwnershipEpoch::initial());

        // Owner transfer advances both version and fencing epoch.
        let moved = current.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]);
        let outcome = catalog.apply_update(moved).unwrap();
        assert_eq!(outcome, UpdateOutcome::Updated);

        let after = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(after.owner(), &ident("CN=node-b"));
        assert_eq!(after.version().value(), 2);
        assert_eq!(after.epoch().value(), 2); // old owner is now fenced

        // A replica-set change advances the version but NOT the epoch.
        let replicas_changed = after.update_replicas([ident("CN=node-c")]);
        catalog.apply_update(replicas_changed).unwrap();
        let after2 = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(after2.version().value(), 3);
        assert_eq!(after2.epoch().value(), 2); // write authority did not move
        assert_eq!(after2.replicas(), &[ident("CN=node-c")]);
    }

    #[test]
    fn stale_update_is_rejected_and_leaves_catalog_unchanged() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(hash_range(&orders, 1, RangeBounds::full(), "CN=node-a"))
            .unwrap();

        let v1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        // Advance to v2.
        catalog
            .apply_update(v1.transfer_to(ident("CN=node-b"), []))
            .unwrap();
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-b")
        );

        // Re-applying the original v1 entry (and even a fresh v1-versioned write)
        // is stale: version 1 does not advance past the current version 2.
        let err = catalog.apply_update(v1.clone()).unwrap_err();
        assert_eq!(
            err,
            CatalogError::StaleVersion {
                collection: orders.clone(),
                range_id: RangeId::new(1),
                current: CatalogVersion::initial().next(),
                attempted: CatalogVersion::initial(),
            }
        );
        // The stale write did not roll ownership back.
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-b")
        );
        assert_eq!(
            catalog
                .range(&orders, RangeId::new(1))
                .unwrap()
                .version()
                .value(),
            2
        );
    }

    #[test]
    fn overlapping_range_creation_is_rejected() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(hash_range(
                &orders,
                1,
                bounds(&[0x00], &[0x80]),
                "CN=node-a",
            ))
            .unwrap();
        // A new range overlapping [0x00, 0x80) is ambiguous for routing.
        let err = catalog
            .apply_update(hash_range(
                &orders,
                2,
                bounds(&[0x40], &[0xc0]),
                "CN=node-b",
            ))
            .unwrap_err();
        assert_eq!(
            err,
            CatalogError::OverlappingRange {
                collection: orders.clone(),
                existing: RangeId::new(1),
                attempted: RangeId::new(2),
            }
        );
        assert_eq!(catalog.range_count(), 1);
    }

    #[test]
    fn overlapping_range_update_is_rejected() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(hash_range(
                &orders,
                1,
                bounds(&[0x00], &[0x80]),
                "CN=node-a",
            ))
            .unwrap();
        catalog
            .apply_update(hash_range(
                &orders,
                2,
                RangeBounds::new(RangeBound::key([0x80]), RangeBound::Max).unwrap(),
                "CN=node-b",
            ))
            .unwrap();

        let widened = catalog
            .range(&orders, RangeId::new(1))
            .unwrap()
            .with_bounds(bounds(&[0x00], &[0xc0]));
        let err = catalog.apply_update(widened).unwrap_err();
        assert_eq!(
            err,
            CatalogError::OverlappingRange {
                collection: orders.clone(),
                existing: RangeId::new(2),
                attempted: RangeId::new(1),
            }
        );
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().bounds(),
            &bounds(&[0x00], &[0x80])
        );
    }

    #[test]
    fn catalog_replicates_to_data_members_with_read_visibility() {
        // Leader writes the catalog; a data member holds its own replica and
        // applies the same versioned updates — no user-data sharding involved.
        let orders = collection("orders");
        let mut leader = ShardOwnershipCatalog::new();
        let mut data_member = ShardOwnershipCatalog::new();

        // Leader creates a range; ship the entry to the data member.
        let create = hash_range(&orders, 1, RangeBounds::full(), "CN=node-a");
        leader.apply_update(create.clone()).unwrap();
        assert_eq!(
            data_member.apply_update(create).unwrap(),
            UpdateOutcome::Created
        );

        // The data member can route locally to the same owner the leader has.
        assert_eq!(
            data_member.route(&orders, b"any-key").unwrap().owner(),
            &ident("CN=node-a")
        );

        // Leader transfers ownership; replicate the v2 entry.
        let v2 = leader
            .range(&orders, RangeId::new(1))
            .unwrap()
            .transfer_to(ident("CN=node-b"), []);
        leader.apply_update(v2.clone()).unwrap();
        assert_eq!(
            data_member.apply_update(v2.clone()).unwrap(),
            UpdateOutcome::Updated
        );
        assert_eq!(
            data_member.route(&orders, b"any-key").unwrap().owner(),
            &ident("CN=node-b")
        );

        // Out-of-order / duplicate replication: re-delivering v2 after it is
        // applied is stale on the replica and rejected, so it stays consistent.
        let err = data_member.apply_update(v2).unwrap_err();
        assert!(matches!(err, CatalogError::StaleVersion { .. }));
        assert_eq!(
            data_member
                .range(&orders, RangeId::new(1))
                .unwrap()
                .version()
                .value(),
            2
        );
    }

    #[test]
    fn range_bounds_reject_empty_or_inverted() {
        assert!(RangeBounds::new(RangeBound::key([0x10]), RangeBound::key([0x10])).is_err());
        assert!(RangeBounds::new(RangeBound::key([0x20]), RangeBound::key([0x10])).is_err());
        assert!(RangeBounds::new(RangeBound::Max, RangeBound::Min).is_err());
        assert!(RangeBounds::full().contains(b"anything"));
    }

    // ---------------------------------------------------------------
    // Issue #990 — per-range role model and ownership-aware write gate.
    // ---------------------------------------------------------------

    /// A range owned by `owner` with an explicit replica set.
    fn range_with(
        coll: &CollectionId,
        id: u64,
        bnds: RangeBounds,
        owner: &str,
        replicas: &[&str],
    ) -> RangeOwnership {
        RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Hash,
            bnds,
            ident(owner),
            replicas.iter().map(|r| ident(r)).collect::<Vec<_>>(),
            PlacementMetadata::with_replication_factor(3),
        )
    }

    #[test]
    fn role_of_distinguishes_owner_replica_and_no_copy() {
        let orders = collection("orders");
        let range = range_with(&orders, 1, RangeBounds::full(), "CN=node-a", &["CN=node-b"]);

        assert_eq!(range.role_of(&ident("CN=node-a")), RangeRole::Owner);
        assert_eq!(range.role_of(&ident("CN=node-b")), RangeRole::Replica);
        assert_eq!(range.role_of(&ident("CN=node-c")), RangeRole::NoCopy);
        assert!(RangeRole::Owner.may_write_public());
        assert!(!RangeRole::Replica.may_write_public());
        assert!(!RangeRole::NoCopy.may_write_public());
    }

    #[test]
    fn role_is_per_range_not_a_global_node_role() {
        // node-a owns range 1 and is only a replica of range 2 — the same node
        // holds different roles for different ranges of the same collection.
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(range_with(
                &orders,
                1,
                RangeBounds::new(RangeBound::Min, RangeBound::key([0x80])).unwrap(),
                "CN=node-a",
                &["CN=node-b"],
            ))
            .unwrap();
        catalog
            .apply_update(range_with(
                &orders,
                2,
                RangeBounds::new(RangeBound::key([0x80]), RangeBound::Max).unwrap(),
                "CN=node-b",
                &["CN=node-a"],
            ))
            .unwrap();

        let node_a = ident("CN=node-a");
        assert_eq!(
            catalog.role_at(&node_a, &orders, RangeId::new(1)),
            Some(RangeRole::Owner)
        );
        assert_eq!(
            catalog.role_at(&node_a, &orders, RangeId::new(2)),
            Some(RangeRole::Replica)
        );
        // A range that does not exist is None, not NoCopy.
        assert_eq!(catalog.role_at(&node_a, &orders, RangeId::new(99)), None);
        // And a collection nobody placed yet routes nowhere.
        assert_eq!(
            catalog.role_at(&node_a, &collection("ghost"), RangeId::new(1)),
            None
        );
    }

    #[test]
    fn public_write_admitted_on_owner_at_matching_epoch() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(range_with(
                &orders,
                1,
                RangeBounds::full(),
                "CN=node-a",
                &["CN=node-b"],
            ))
            .unwrap();

        let admitted = catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .expect("owner at current epoch may write");
        assert_eq!(admitted.owner(), &ident("CN=node-a"));
        assert_eq!(admitted.range_id(), RangeId::new(1));
    }

    #[test]
    fn public_write_uses_hash_slot_routing_for_hash_collections() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let key = b"tenant:42";
        catalog
            .apply_update(hash_range(
                &orders,
                1,
                single_hash_slot_bounds(key),
                "CN=node-a",
            ))
            .unwrap();

        let admitted = catalog
            .admit_public_write(&ident("CN=node-a"), &orders, key, OwnershipEpoch::initial())
            .expect("hash owner admits write routed by shard-key slot");
        assert_eq!(admitted.range_id(), RangeId::new(1));
    }

    #[test]
    fn public_write_rejected_on_replica_with_routing_error() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(range_with(
                &orders,
                1,
                RangeBounds::full(),
                "CN=node-a",
                &["CN=node-b"],
            ))
            .unwrap();

        // node-b holds a copy but is a replica — a public write must be routed
        // to the owner, not applied locally.
        let err = catalog
            .admit_public_write(
                &ident("CN=node-b"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        match err {
            RangeWriteReject::NotOwner {
                role, ref owner, ..
            } => {
                assert_eq!(role, RangeRole::Replica);
                assert_eq!(owner, &ident("CN=node-a"));
            }
            other => panic!("expected NotOwner(Replica), got {other:?}"),
        }
        // The rejection names the owner so the caller can re-route.
        assert!(err.to_string().contains("route the write to"));
    }

    #[test]
    fn public_write_rejected_on_no_copy_holder() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(range_with(
                &orders,
                1,
                RangeBounds::full(),
                "CN=node-a",
                &["CN=node-b"],
            ))
            .unwrap();

        // node-c holds no copy of the range at all.
        let err = catalog
            .admit_public_write(
                &ident("CN=node-c"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        match err {
            RangeWriteReject::NotOwner { role, .. } => assert_eq!(role, RangeRole::NoCopy),
            other => panic!("expected NotOwner(NoCopy), got {other:?}"),
        }
    }

    #[test]
    fn public_write_rejected_on_stale_ownership_epoch() {
        // Ownership moved a→b and back to a, advancing the epoch twice. A write
        // the routing layer authorised under the original epoch must be fenced
        // even though node-a is, once again, the current owner.
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let v1 = range_with(&orders, 1, RangeBounds::full(), "CN=node-a", &["CN=node-b"]);
        let original_epoch = v1.epoch();
        catalog.apply_update(v1.clone()).unwrap();

        let v2 = v1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]);
        catalog.apply_update(v2.clone()).unwrap();
        let v3 = v2.transfer_to(ident("CN=node-a"), [ident("CN=node-b")]);
        catalog.apply_update(v3.clone()).unwrap();

        // node-a is the current owner again, but at a newer epoch.
        assert_ne!(original_epoch, v3.epoch());
        let err = catalog
            .admit_public_write(&ident("CN=node-a"), &orders, b"k", original_epoch)
            .unwrap_err();
        match err {
            RangeWriteReject::StaleEpoch {
                expected, current, ..
            } => {
                assert_eq!(expected, original_epoch);
                assert_eq!(current, v3.epoch());
            }
            other => panic!("expected StaleEpoch, got {other:?}"),
        }
        // The same owner at the *current* epoch is admitted.
        assert!(catalog
            .admit_public_write(&ident("CN=node-a"), &orders, b"k", v3.epoch())
            .is_ok());
    }

    #[test]
    fn public_write_rejected_when_no_range_covers_the_key() {
        let catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let err = catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        assert!(matches!(err, RangeWriteReject::NoRange { .. }));
    }

    #[test]
    fn internal_apply_path_stays_privileged_for_a_public_write_replica() {
        // A node that rejects a *public* write because it is only a replica must
        // still admit the owner's replicated changes through the privileged
        // internal apply path — that path is gated by issue #991's range
        // authority fence, not by this public ownership gate.
        use crate::replication::cdc::{ChangeOperation, ChangeRecord, RangeAuthority};

        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        catalog
            .apply_update(range_with(
                &orders,
                7,
                RangeBounds::full(),
                "CN=node-a",
                &["CN=node-b"],
            ))
            .unwrap();

        // Public gate: node-b is a replica → rejected, never reaches storage.
        assert!(matches!(
            catalog
                .admit_public_write(
                    &ident("CN=node-b"),
                    &orders,
                    b"k",
                    OwnershipEpoch::initial()
                )
                .unwrap_err(),
            RangeWriteReject::NotOwner {
                role: RangeRole::Replica,
                ..
            }
        ));

        // Internal apply: the owner's replicated change for the same range is
        // admitted by the range-authority fence on the replica.
        let record = ChangeRecord {
            term: 1,
            lsn: 1,
            timestamp: 0,
            operation: ChangeOperation::Insert,
            collection: orders.as_str().to_string(),
            entity_id: 1,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1]),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        }
        .with_range_authority(7, OwnershipEpoch::initial().value());
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 1,
            min_ownership_epoch: OwnershipEpoch::initial().value(),
        };
        assert!(
            fence.admit(&record).is_ok(),
            "replica internal apply must remain privileged for the owner's changes"
        );
    }
}
