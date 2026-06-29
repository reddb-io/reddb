//! Topology refresh and routing-hint client contract (issue #994, PRD #987, ADR 0037).
//!
//! Any-node routing ([`plan_route`](ShardOwnershipCatalog::plan_route), issue
//! #993) lets a request land on any data member and still do something correct.
//! This module is the *client-facing* half of that story: the contract a driver
//! uses to learn the cluster's shape, route directly to range owners, and stay
//! correct as ownership moves. Three mechanisms, in strict priority of authority:
//!
//! 1. **Polling** is the baseline and the source of authority. A driver fetches a
//!    [`TopologySnapshot`] — every range's bounds, owner, replicas, ownership
//!    epoch, and catalog version — and caches it in a [`ClientTopology`]. This is
//!    the only path that establishes *authoritative* topology, and a driver that
//!    only ever polls is always eventually correct.
//!
//! 2. **Routing hints** ([`RoutingHint`](super::routing::RoutingHint), carried on
//!    a redirect response from issue #993) are an *advisory correction*. When a
//!    write reaches a stale owner the response names the current owner+epoch; the
//!    driver applies that hint to stop hammering the stale node, but the hint is
//!    explicitly **not** authoritative — it cannot introduce ranges, it carries no
//!    replica set, and applying one raises [`needs_refresh`](ClientTopology::needs_refresh)
//!    so the driver knows to reconcile against an authoritative poll. This is
//!    ADR 0037's "stale ownership responses remain the mandatory correctness path"
//!    expressed on the client: correctness never *depends* on a hint, a hint only
//!    *accelerates* convergence.
//!
//! 3. **Push / subscription updates** ([`TopologyUpdate`]) are an optional
//!    accelerator where the transport supports them. A pushed snapshot or
//!    single-range delta flows through the *same monotonic apply path* as a poll,
//!    so a driver that misses a push is never wrong — the next poll (or the next
//!    redirect hint) carries it forward. Push is never mandatory for correctness.
//!
//! Like the rest of the cluster module this is a pure data/decision layer with no
//! I/O: [`topology_snapshot`](ShardOwnershipCatalog::topology_snapshot) projects a
//! catalog into a driver-facing payload, and [`ClientTopology`] models exactly how
//! a driver folds polls, hints, and pushes together. The transport that serialises
//! a snapshot onto the wire or pushes a delta is a separate concern on top.

use std::collections::BTreeMap;

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogVersion, CollectionId, OwnershipEpoch, RangeBounds, RangeId, RangeOwnership,
    ReplicaRole, ShardKeyMode, ShardOwnershipCatalog,
};
use super::routing::RoutingHint;
use super::slot::hash_shard_key_to_range_key;

/// One range's routing metadata as a driver sees it.
///
/// Carries everything a driver needs to route a key directly to its owner and
/// fence the write at the right epoch: the half-open [`bounds`](Self::bounds) for
/// client-side range routing, the [`owner`](Self::owner) to send to, the
/// [`replicas`](Self::replicas) for read fan-out, the [`epoch`](Self::epoch) to
/// stamp a write at, and the [`version`](Self::version) used to decide whether an
/// incoming update is newer than what is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyRange {
    collection: CollectionId,
    range_id: RangeId,
    shard_key_mode: ShardKeyMode,
    bounds: RangeBounds,
    owner: NodeIdentity,
    replicas: Vec<NodeIdentity>,
    compressed_archive_replicas: Vec<NodeIdentity>,
    epoch: OwnershipEpoch,
    version: CatalogVersion,
}

impl TopologyRange {
    fn from_ownership(range: &RangeOwnership) -> Self {
        Self {
            collection: range.collection().clone(),
            range_id: range.range_id(),
            shard_key_mode: range.shard_key_mode(),
            bounds: range.bounds().clone(),
            owner: range.owner().clone(),
            replicas: range.hot_mirror_replicas().to_vec(),
            compressed_archive_replicas: range.compressed_archive_replicas().to_vec(),
            epoch: range.epoch(),
            version: range.version(),
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

    pub fn hot_mirror_replicas(&self) -> &[NodeIdentity] {
        &self.replicas
    }

    pub fn compressed_archive_replicas(&self) -> &[NodeIdentity] {
        &self.compressed_archive_replicas
    }

    pub fn replica_role_of(&self, node: &NodeIdentity) -> Option<ReplicaRole> {
        if self.replicas.iter().any(|replica| replica == node) {
            Some(ReplicaRole::HotMirror)
        } else if self
            .compressed_archive_replicas
            .iter()
            .any(|replica| replica == node)
        {
            Some(ReplicaRole::CompressedArchive)
        } else {
            None
        }
    }

    pub fn promotion_candidates(&self) -> &[NodeIdentity] {
        &self.replicas
    }

    /// The epoch a driver should stamp a write to this range at — the same epoch
    /// the owner's [`admit_public_write`](ShardOwnershipCatalog::admit_public_write)
    /// gate will check (issue #990).
    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }

    pub fn version(&self) -> CatalogVersion {
        self.version
    }

    fn key(&self) -> (CollectionId, RangeId) {
        (self.collection.clone(), self.range_id)
    }
}

/// A point-in-time, driver-facing projection of the ownership catalog — the
/// payload a topology poll returns.
///
/// The [`version`](Self::version) is the snapshot's high-water mark: the
/// **maximum** catalog version across its ranges (or [`CatalogVersion::initial`]
/// for an empty cluster). It is monotonic, but not a complete generation number:
/// two different ranges can independently advance to the same version. Drivers
/// therefore use it as a cheap stale-snapshot guard and still compare per-range
/// content for same-version full refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologySnapshot {
    version: CatalogVersion,
    ranges: Vec<TopologyRange>,
}

impl TopologySnapshot {
    /// The snapshot generation — the high-water catalog version across its ranges.
    pub fn version(&self) -> CatalogVersion {
        self.version
    }

    /// Every range in the snapshot, in `(collection, range_id)` order.
    pub fn ranges(&self) -> &[TopologyRange] {
        &self.ranges
    }

    /// Look up one range by identity.
    pub fn range(&self, collection: &CollectionId, range_id: RangeId) -> Option<&TopologyRange> {
        self.ranges
            .iter()
            .find(|r| r.collection() == collection && r.range_id() == range_id)
    }

    /// Route a normalized range key to the range that owns it, by the same
    /// half-open containment predicate the server uses.
    pub fn route(&self, collection: &CollectionId, key: &[u8]) -> Option<&TopologyRange> {
        self.ranges
            .iter()
            .find(|r| r.collection() == collection && r.bounds().contains(key))
    }

    /// Route a logical shard key through the collection's shard-key mode.
    pub fn route_shard_key(
        &self,
        collection: &CollectionId,
        shard_key: &[u8],
    ) -> Option<&TopologyRange> {
        match self.shard_key_mode(collection)? {
            ShardKeyMode::Ordered => self.route(collection, shard_key),
            ShardKeyMode::Hash => {
                let range_key = hash_shard_key_to_range_key(shard_key);
                self.route(collection, &range_key)
            }
        }
    }

    fn shard_key_mode(&self, collection: &CollectionId) -> Option<ShardKeyMode> {
        self.ranges
            .iter()
            .find(|range| range.collection() == collection)
            .map(TopologyRange::shard_key_mode)
    }
}

impl ShardOwnershipCatalog {
    /// Project the catalog into a driver-facing [`TopologySnapshot`] — the payload
    /// a topology poll serves (issue #994).
    ///
    /// The snapshot carries every range's bounds, owner, replicas, ownership
    /// epoch, and catalog version, and stamps a generation
    /// ([`version`](TopologySnapshot::version)) drivers use to tell a newer
    /// snapshot from a stale one.
    pub fn topology_snapshot(&self) -> TopologySnapshot {
        let ranges: Vec<TopologyRange> =
            self.entries().map(TopologyRange::from_ownership).collect();
        let version = ranges
            .iter()
            .map(TopologyRange::version)
            .max()
            .unwrap_or_else(CatalogVersion::initial);
        TopologySnapshot { version, ranges }
    }
}

/// The result of folding a polled or pushed snapshot/delta into a [`ClientTopology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The incoming data was strictly newer and was adopted; `ranges_changed`
    /// ranges were added or advanced.
    Applied { ranges_changed: usize },
    /// The incoming data was not newer than what is cached and was ignored — the
    /// monotonicity guard that makes out-of-order and duplicate delivery safe.
    Ignored,
}

impl RefreshOutcome {
    pub fn was_applied(self) -> bool {
        matches!(self, RefreshOutcome::Applied { .. })
    }
}

/// The result of applying an advisory [`RoutingHint`](super::routing::RoutingHint)
/// correction to a [`ClientTopology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintOutcome {
    /// The hint named a newer owner/epoch for a known range; the cached owner was
    /// corrected and [`needs_refresh`](ClientTopology::needs_refresh) was raised.
    Corrected,
    /// The cache already held this range at or beyond the hint's version; nothing
    /// changed (an authoritative apply had already overtaken the hint).
    AlreadyCurrent,
    /// The hint named a range the cache does not know. A hint is never
    /// authoritative enough to *introduce* a range, so no range was created;
    /// [`needs_refresh`](ClientTopology::needs_refresh) was raised so the driver
    /// polls for the authoritative topology.
    UnknownRange,
}

/// A topology change delivered over a push / subscription transport.
///
/// Both variants flow through the same monotonic apply path as a poll
/// ([`ClientTopology::apply_update`]), so a missed or out-of-order push is never a
/// correctness problem — it is only a missed *acceleration*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyUpdate {
    /// A full snapshot push — identical in effect to a poll.
    Full(TopologySnapshot),
    /// A single range advanced; carries just that range's new metadata.
    Range(TopologyRange),
}

/// A driver's cached view of cluster topology, and the contract for keeping it
/// correct (issue #994).
///
/// Holds the ranges the driver believes in, the authoritative
/// [`version`](Self::version) of the last *polled or pushed* snapshot, and a
/// [`needs_refresh`](Self::needs_refresh) flag that is raised whenever the driver
/// is running on an advisory hint correction rather than authoritative topology.
///
/// The three inputs compose by authority: [`apply_refresh`](Self::apply_refresh)
/// and [`apply_update`](Self::apply_update) are authoritative and monotonic;
/// [`apply_hint`](Self::apply_hint) is advisory and only ever corrects a *known*
/// range's owner/epoch. Authority always wins — an authoritative apply that is
/// newer overwrites a prior hint correction and clears `needs_refresh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTopology {
    version: CatalogVersion,
    ranges: BTreeMap<(CollectionId, RangeId), TopologyRange>,
    needs_refresh: bool,
}

impl ClientTopology {
    /// Seed a cache from an initial topology poll.
    pub fn from_snapshot(snapshot: TopologySnapshot) -> Self {
        let mut cache = Self {
            version: snapshot.version(),
            ranges: BTreeMap::new(),
            needs_refresh: false,
        };
        for range in snapshot.ranges {
            cache.ranges.insert(range.key(), range);
        }
        cache
    }

    /// The authoritative generation of this cache — the version of the most recent
    /// snapshot or range delta adopted. Advisory hint corrections do **not** move
    /// it, so it always reflects authoritative topology.
    pub fn version(&self) -> CatalogVersion {
        self.version
    }

    /// Whether the cache is running on an advisory hint correction and should poll
    /// for authoritative topology. Raised by [`apply_hint`](Self::apply_hint);
    /// cleared by an authoritative [`apply_refresh`](Self::apply_refresh) (or a
    /// full-snapshot [`apply_update`](Self::apply_update)).
    pub fn needs_refresh(&self) -> bool {
        self.needs_refresh
    }

    /// The cached range owning a normalized range key.
    pub fn route(&self, collection: &CollectionId, key: &[u8]) -> Option<&TopologyRange> {
        self.ranges
            .values()
            .find(|r| r.collection() == collection && r.bounds().contains(key))
    }

    /// The owner a driver should send a request for `key` to — the routing answer.
    pub fn resolve(&self, collection: &CollectionId, key: &[u8]) -> Option<&NodeIdentity> {
        self.route_shard_key(collection, key)
            .map(TopologyRange::owner)
    }

    /// The cached range owning a logical shard key.
    pub fn route_shard_key(
        &self,
        collection: &CollectionId,
        shard_key: &[u8],
    ) -> Option<&TopologyRange> {
        match self.shard_key_mode(collection)? {
            ShardKeyMode::Ordered => self.route(collection, shard_key),
            ShardKeyMode::Hash => {
                let range_key = hash_shard_key_to_range_key(shard_key);
                self.route(collection, &range_key)
            }
        }
    }

    fn shard_key_mode(&self, collection: &CollectionId) -> Option<ShardKeyMode> {
        self.ranges
            .values()
            .find(|range| range.collection() == collection)
            .map(TopologyRange::shard_key_mode)
    }

    /// One cached range by identity.
    pub fn range(&self, collection: &CollectionId, range_id: RangeId) -> Option<&TopologyRange> {
        self.ranges.get(&(collection.clone(), range_id))
    }

    /// Adopt a freshly polled snapshot — the authoritative refresh path.
    ///
    /// Monotonic: the snapshot is adopted if its generation advances the cache's
    /// authoritative [`version`](Self::version), or if it carries same-generation
    /// range content that does not roll any cached range backwards. An adopted
    /// snapshot replaces the cached ranges wholesale and clears
    /// [`needs_refresh`](Self::needs_refresh); an older or duplicate one is
    /// [`Ignored`](RefreshOutcome::Ignored).
    pub fn apply_refresh(&mut self, snapshot: TopologySnapshot) -> RefreshOutcome {
        if !self.ranges.is_empty() && snapshot.version() < self.version {
            return RefreshOutcome::Ignored;
        }
        if self.snapshot_rolls_back_any_range(&snapshot) {
            return RefreshOutcome::Ignored;
        }
        let mut changed = 0usize;
        let mut next: BTreeMap<(CollectionId, RangeId), TopologyRange> = BTreeMap::new();
        for range in snapshot.ranges {
            let key = range.key();
            if self.ranges.get(&key) != Some(&range) {
                changed += 1;
            }
            next.insert(key, range);
        }
        if !self.ranges.is_empty() && snapshot.version <= self.version && changed == 0 {
            return RefreshOutcome::Ignored;
        }
        self.ranges = next;
        self.version = snapshot.version;
        self.needs_refresh = false;
        RefreshOutcome::Applied {
            ranges_changed: changed,
        }
    }

    fn snapshot_rolls_back_any_range(&self, snapshot: &TopologySnapshot) -> bool {
        snapshot.ranges().iter().any(|incoming| {
            self.ranges
                .get(&incoming.key())
                .is_some_and(|current| incoming.version() < current.version())
        })
    }

    /// Fold a pushed topology update in — the optional push/subscription path.
    ///
    /// A [`Full`](TopologyUpdate::Full) push is exactly an
    /// [`apply_refresh`](Self::apply_refresh). A [`Range`](TopologyUpdate::Range)
    /// delta advances a single range when its version is newer than the cached
    /// one (and bumps the authoritative version to match), or is
    /// [`Ignored`](RefreshOutcome::Ignored) when it is not newer. Because both go
    /// through the same monotonic guard, a missed push is never a correctness
    /// problem — a later poll or delta carries the change forward.
    pub fn apply_update(&mut self, update: TopologyUpdate) -> RefreshOutcome {
        match update {
            TopologyUpdate::Full(snapshot) => self.apply_refresh(snapshot),
            TopologyUpdate::Range(range) => {
                let key = range.key();
                let newer = match self.ranges.get(&key) {
                    Some(current) => range.version() > current.version(),
                    None => true,
                };
                if !newer {
                    return RefreshOutcome::Ignored;
                }
                if range.version() > self.version {
                    self.version = range.version();
                }
                self.ranges.insert(key, range);
                RefreshOutcome::Applied { ranges_changed: 1 }
            }
        }
    }

    /// Apply an advisory routing-hint correction from a redirect response — the
    /// stale-ownership correctness path (issue #993, ADR 0037).
    ///
    /// A hint is **not** authoritative: it can only correct the owner/epoch of a
    /// range the cache already knows, and only when it is strictly newer than the
    /// cached range. On a correction the cached owner/epoch/version advance (the
    /// known bounds are kept; the replica set is left as-is because a hint carries
    /// none) and [`needs_refresh`](Self::needs_refresh) is raised so the driver
    /// reconciles against an authoritative poll. A hint for an unknown range
    /// creates nothing — it only raises `needs_refresh`.
    pub fn apply_hint(&mut self, hint: &RoutingHint) -> HintOutcome {
        let key = (hint.collection().clone(), hint.range_id());
        match self.ranges.get_mut(&key) {
            Some(range) => {
                if hint.version() <= range.version {
                    return HintOutcome::AlreadyCurrent;
                }
                range.owner = hint.owner().clone();
                range.epoch = hint.epoch();
                range.version = hint.version();
                self.needs_refresh = true;
                HintOutcome::Corrected
            }
            None => {
                self.needs_refresh = true;
                HintOutcome::UnknownRange
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{PlacementMetadata, RangeBound, ShardKeyMode};
    use crate::cluster::routing::{RequestOperation, RouteDecision, RoutedRequest, RoutingPolicy};

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn full_range(coll: &CollectionId, id: u64, owner: &str, replicas: &[&str]) -> RangeOwnership {
        RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Hash,
            RangeBounds::full(),
            ident(owner),
            replicas.iter().map(|r| ident(r)).collect::<Vec<_>>(),
            PlacementMetadata::with_replication_factor(3),
        )
    }

    fn split_range(
        coll: &CollectionId,
        id: u64,
        lower: RangeBound,
        upper: RangeBound,
        owner: &str,
    ) -> RangeOwnership {
        RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Ordered,
            RangeBounds::new(lower, upper).unwrap(),
            ident(owner),
            Vec::<NodeIdentity>::new(),
            PlacementMetadata::with_replication_factor(1),
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

    fn hash_slot_range(
        coll: &CollectionId,
        id: u64,
        shard_key: &[u8],
        owner: &str,
    ) -> RangeOwnership {
        RangeOwnership::establish(
            coll.clone(),
            RangeId::new(id),
            ShardKeyMode::Hash,
            single_hash_slot_bounds(shard_key),
            ident(owner),
            Vec::<NodeIdentity>::new(),
            PlacementMetadata::with_replication_factor(1),
        )
    }

    fn catalog_with(ranges: impl IntoIterator<Item = RangeOwnership>) -> ShardOwnershipCatalog {
        let mut catalog = ShardOwnershipCatalog::new();
        for range in ranges {
            catalog.apply_update(range).unwrap();
        }
        catalog
    }

    // AC #1: a topology payload carries enough per-range routing metadata (owner,
    // replicas, epoch, version, bounds) for a driver to route directly.
    #[test]
    fn snapshot_exposes_routing_metadata_for_direct_routing() {
        let orders = collection("orders");
        let catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);

        let snapshot = catalog.topology_snapshot();
        assert_eq!(snapshot.version(), CatalogVersion::initial());
        assert_eq!(snapshot.ranges().len(), 1);

        let range = snapshot
            .route(&orders, b"any-key")
            .expect("full range covers all keys");
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.replicas(), &[ident("CN=node-b")]);
        assert_eq!(range.epoch(), OwnershipEpoch::initial());
        assert_eq!(range.range_id(), RangeId::new(1));
    }

    #[test]
    fn snapshot_exposes_explicit_replica_roles() {
        let orders = collection("orders");
        let catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])
            .with_compressed_archive_replicas([ident("CN=node-c")])]);

        let snapshot = catalog.topology_snapshot();
        let range = snapshot
            .route(&orders, b"any-key")
            .expect("full range covers all keys");

        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.hot_mirror_replicas(), &[ident("CN=node-b")]);
        assert_eq!(range.compressed_archive_replicas(), &[ident("CN=node-c")]);
        assert_eq!(range.promotion_candidates(), &[ident("CN=node-b")]);
        assert_eq!(
            range.replica_role_of(&ident("CN=node-b")),
            Some(ReplicaRole::HotMirror)
        );
        assert_eq!(
            range.replica_role_of(&ident("CN=node-c")),
            Some(ReplicaRole::CompressedArchive)
        );
    }

    // AC #1: an ordered, multi-range collection routes keys to distinct owners
    // entirely client-side from the snapshot.
    #[test]
    fn snapshot_routes_keys_to_distinct_owners() {
        let parts = collection("parts");
        let catalog = catalog_with([
            split_range(
                &parts,
                1,
                RangeBound::Min,
                RangeBound::key(b"m"),
                "CN=node-a",
            ),
            split_range(
                &parts,
                2,
                RangeBound::key(b"m"),
                RangeBound::Max,
                "CN=node-b",
            ),
        ]);
        let snapshot = catalog.topology_snapshot();

        assert_eq!(
            snapshot.route(&parts, b"apple").unwrap().owner(),
            &ident("CN=node-a")
        );
        assert_eq!(
            snapshot.route(&parts, b"zebra").unwrap().owner(),
            &ident("CN=node-b")
        );
    }

    // AC #3: a driver polls a snapshot and resolves owners from its cache.
    #[test]
    fn client_resolves_owner_from_polled_snapshot() {
        let orders = collection("orders");
        let catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &[])]);
        let client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-a"));
        assert!(!client.needs_refresh());
    }

    #[test]
    fn client_resolves_hash_collection_by_shard_key_slot() {
        let orders = collection("orders");
        let key = b"tenant:42";
        let catalog = catalog_with([hash_slot_range(&orders, 1, key, "CN=node-a")]);
        let client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        let routed = client
            .route_shard_key(&orders, key)
            .expect("hash slot range covers the logical shard key");
        assert_eq!(routed.owner(), &ident("CN=node-a"));
        assert_eq!(client.resolve(&orders, key).unwrap(), &ident("CN=node-a"));
    }

    // AC #3 + polling baseline: refresh is monotonic — a newer poll is adopted, a
    // stale or duplicate poll is ignored and cannot roll the cache backwards.
    #[test]
    fn refresh_is_monotonic() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());
        let v1 = client.version();

        // Ownership transfers a -> b; poll the new snapshot.
        let r = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();
        let fresh = catalog.topology_snapshot();
        assert!(fresh.version() > v1);

        assert_eq!(
            client.apply_refresh(fresh.clone()),
            RefreshOutcome::Applied { ranges_changed: 1 }
        );
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-b"));

        // Re-applying the same snapshot is a no-op.
        assert_eq!(client.apply_refresh(fresh), RefreshOutcome::Ignored);
    }

    #[test]
    fn refresh_applies_same_generation_snapshot_when_another_range_changed() {
        let parts = collection("parts");
        let mut catalog = catalog_with([
            split_range(
                &parts,
                1,
                RangeBound::Min,
                RangeBound::key(b"m"),
                "CN=node-a",
            ),
            split_range(
                &parts,
                2,
                RangeBound::key(b"m"),
                RangeBound::Max,
                "CN=node-b",
            ),
        ]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        let r1 = catalog.range(&parts, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r1.transfer_to(ident("CN=node-c"), Vec::<NodeIdentity>::new()))
            .unwrap();
        assert_eq!(
            client.apply_refresh(catalog.topology_snapshot()),
            RefreshOutcome::Applied { ranges_changed: 1 }
        );
        assert_eq!(
            client.resolve(&parts, b"apple").unwrap(),
            &ident("CN=node-c")
        );

        let r2 = catalog.range(&parts, RangeId::new(2)).unwrap().clone();
        catalog
            .apply_update(r2.transfer_to(ident("CN=node-d"), Vec::<NodeIdentity>::new()))
            .unwrap();
        let same_generation = catalog.topology_snapshot();
        assert_eq!(same_generation.version(), client.version());

        assert_eq!(
            client.apply_refresh(same_generation),
            RefreshOutcome::Applied { ranges_changed: 1 }
        );
        assert_eq!(
            client.resolve(&parts, b"zebra").unwrap(),
            &ident("CN=node-d")
        );
    }

    #[test]
    fn equal_generation_refresh_does_not_roll_back_a_newer_range() {
        let parts = collection("parts");
        let base = catalog_with([
            split_range(
                &parts,
                1,
                RangeBound::Min,
                RangeBound::key(b"m"),
                "CN=node-a",
            ),
            split_range(
                &parts,
                2,
                RangeBound::key(b"m"),
                RangeBound::Max,
                "CN=node-b",
            ),
        ]);
        let mut current_catalog = base.clone();
        let mut stale_fork = base;
        let mut client = ClientTopology::from_snapshot(current_catalog.topology_snapshot());

        let r1 = current_catalog
            .range(&parts, RangeId::new(1))
            .unwrap()
            .clone();
        current_catalog
            .apply_update(r1.transfer_to(ident("CN=node-c"), Vec::<NodeIdentity>::new()))
            .unwrap();
        assert!(client
            .apply_refresh(current_catalog.topology_snapshot())
            .was_applied());

        let r2 = stale_fork.range(&parts, RangeId::new(2)).unwrap().clone();
        stale_fork
            .apply_update(r2.transfer_to(ident("CN=node-d"), Vec::<NodeIdentity>::new()))
            .unwrap();
        let fork_snapshot = stale_fork.topology_snapshot();
        assert_eq!(fork_snapshot.version(), client.version());

        assert_eq!(client.apply_refresh(fork_snapshot), RefreshOutcome::Ignored);
        assert_eq!(
            client.resolve(&parts, b"apple").unwrap(),
            &ident("CN=node-c")
        );
        assert_eq!(
            client.resolve(&parts, b"zebra").unwrap(),
            &ident("CN=node-b")
        );
    }

    // AC #2 + AC #3: a stale-ownership redirect hint corrects the cache without
    // being authoritative — it advances the owner but raises needs_refresh.
    #[test]
    fn redirect_hint_corrects_cache_but_is_not_authoritative() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // Ownership moves a -> b on the server; the driver has not polled yet.
        let r = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();

        // The driver routes to its stale owner (node-a); the server redirects.
        let stale_owner = client.resolve(&orders, b"k").unwrap().clone();
        assert_eq!(stale_owner, ident("CN=node-a"));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
        let hint = match catalog.plan_route(&stale_owner, &request, &RoutingPolicy::forwarding()) {
            RouteDecision::Redirect { hint, .. } => hint,
            other => panic!("expected redirect, got {other:?}"),
        };

        // Applying the hint corrects routing but flags the cache as advisory.
        assert_eq!(client.apply_hint(&hint), HintOutcome::Corrected);
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-b"));
        assert!(
            client.needs_refresh(),
            "a hint is advisory, not authoritative"
        );

        // An authoritative poll reconciles and clears the advisory flag.
        assert!(client
            .apply_refresh(catalog.topology_snapshot())
            .was_applied());
        assert!(!client.needs_refresh());
        // Authority restored the replica set a hint never carried.
        let range = client.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.replicas(), &[ident("CN=node-a")]);
    }

    // AC #2: a hint cannot introduce a range — hints are not the source of
    // ownership authority, so an unknown-range hint only forces a refresh.
    #[test]
    fn hint_for_unknown_range_does_not_invent_topology() {
        let orders = collection("orders");
        let other = collection("other");
        let catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &[])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // Forge a hint for a collection/range the cache has never seen.
        let foreign = catalog_with([full_range(&other, 9, "CN=node-z", &[])]);
        let request =
            RoutedRequest::new(other.clone(), b"k".to_vec(), RequestOperation::Transaction);
        let hint = foreign
            .plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding())
            .hint()
            .cloned()
            .unwrap();

        assert_eq!(client.apply_hint(&hint), HintOutcome::UnknownRange);
        assert!(
            client.range(&other, RangeId::new(9)).is_none(),
            "no phantom range"
        );
        assert!(client.needs_refresh());
    }

    // AC #2: an authoritative apply that already overtook the hint makes the hint
    // a no-op — authority wins.
    #[test]
    fn stale_hint_is_ignored_after_authoritative_catch_up() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
        // Capture an early hint while a still owns the range.
        let early_hint = catalog
            .plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding())
            .hint()
            .cloned()
            .unwrap();

        // Server advances; driver polls the fresh snapshot authoritatively.
        let r = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // The stale early hint (owner a, epoch 1) must not roll routing back.
        assert_eq!(client.apply_hint(&early_hint), HintOutcome::AlreadyCurrent);
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-b"));
        assert!(!client.needs_refresh());
    }

    // AC #4: a pushed full snapshot is adopted exactly like a poll.
    #[test]
    fn push_full_snapshot_applies_like_a_poll() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &[])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        let r = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();
        let update = TopologyUpdate::Full(catalog.topology_snapshot());

        assert!(client.apply_update(update).was_applied());
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-b"));
    }

    // AC #4: a pushed single-range delta advances just that range.
    #[test]
    fn push_range_delta_advances_one_range() {
        let parts = collection("parts");
        let mut catalog = catalog_with([
            split_range(
                &parts,
                1,
                RangeBound::Min,
                RangeBound::key(b"m"),
                "CN=node-a",
            ),
            split_range(
                &parts,
                2,
                RangeBound::key(b"m"),
                RangeBound::Max,
                "CN=node-b",
            ),
        ]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // Only range 2 moves b -> c.
        let r2 = catalog.range(&parts, RangeId::new(2)).unwrap().clone();
        catalog
            .apply_update(r2.transfer_to(ident("CN=node-c"), Vec::<NodeIdentity>::new()))
            .unwrap();
        let moved = catalog
            .topology_snapshot()
            .range(&parts, RangeId::new(2))
            .unwrap()
            .clone();

        assert_eq!(
            client.apply_update(TopologyUpdate::Range(moved)),
            RefreshOutcome::Applied { ranges_changed: 1 }
        );
        // Range 1 untouched, range 2 advanced.
        assert_eq!(
            client.resolve(&parts, b"apple").unwrap(),
            &ident("CN=node-a")
        );
        assert_eq!(
            client.resolve(&parts, b"zebra").unwrap(),
            &ident("CN=node-c")
        );
    }

    // AC #5: behavior when push updates are missed — push is not mandatory for
    // correctness. A dropped push leaves the cache stale, but a redirect hint and
    // a later poll converge it anyway.
    #[test]
    fn missed_push_still_converges_via_hint_and_poll() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // Server moves ownership a -> b. The push for this change is DROPPED:
        // we deliberately never call apply_update with it.
        let r = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();
        let _dropped_push = TopologyUpdate::Full(catalog.topology_snapshot());

        // The cache is stale and still points at node-a.
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-a"));

        // Correctness path: the stale request is redirected; the hint corrects.
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
        let hint = catalog
            .plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding())
            .hint()
            .cloned()
            .unwrap();
        assert_eq!(client.apply_hint(&hint), HintOutcome::Corrected);
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-b"));
        assert!(client.needs_refresh());

        // Baseline poll reconciles the cache fully despite the missed push.
        assert!(client
            .apply_refresh(catalog.topology_snapshot())
            .was_applied());
        assert!(!client.needs_refresh());
    }

    // AC #4: out-of-order pushes are safe — a newer delta applied before an older
    // one wins, and the older one is then ignored.
    #[test]
    fn out_of_order_push_keeps_newest() {
        let orders = collection("orders");
        let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
        let mut client = ClientTopology::from_snapshot(catalog.topology_snapshot());

        // v2: a -> b.
        let r1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();
        let push_v2 = catalog
            .topology_snapshot()
            .range(&orders, RangeId::new(1))
            .unwrap()
            .clone();

        // v3: b -> c.
        let r2 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(r2.transfer_to(ident("CN=node-c"), [ident("CN=node-b")]))
            .unwrap();
        let push_v3 = catalog
            .topology_snapshot()
            .range(&orders, RangeId::new(1))
            .unwrap()
            .clone();

        // The newer push (v3) arrives first and is applied.
        assert!(client
            .apply_update(TopologyUpdate::Range(push_v3))
            .was_applied());
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-c"));
        // The older push (v2) arrives late and is ignored — no rollback.
        assert_eq!(
            client.apply_update(TopologyUpdate::Range(push_v2)),
            RefreshOutcome::Ignored
        );
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-c"));
    }
}
