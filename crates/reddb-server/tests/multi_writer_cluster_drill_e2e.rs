//! End-to-end multi-writer cluster drill (issue #1005, PRD #987).
//!
//! This is the *final verification slice* for the multi-writer cluster: where
//! the per-slice tests (#994/#998/#999/#1000/#1002/#1004) each exercise one rule
//! in isolation, this suite drives the public cluster API end-to-end the way a
//! control plane would, asserting that join, the ownership catalog, routing,
//! leases, failover, forced recovery, drain, rebalancing, and cross-range
//! behaviour compose into one coherent architecture.
//!
//! Acceptance criteria (issue #1005), one test each plus a cohesive whole-drill
//! diagnostics test at the end:
//!
//!  1. A three-data-member cluster joins, advertises topology, creates ranges,
//!     and accepts writes through any-node routing.
//!  2. Different ranges can have different owners while preserving single-writer
//!     authority per range.
//!  3. A stale client receives routing correction or safe forwarding.
//!  4. A range owner failure promotes a replica only when the range commit
//!     watermark is covered.
//!  5. A forced recovery path fences an old owner and emits audit evidence.
//!  6. Drain and rebalancing move ownership without violating write authority.
//!  7. Cross-range write transactions are rejected; simple cross-range read
//!     fanout works.
//!  8. The drill records enough diagnostics to debug membership, ownership
//!     epoch, lease, and WAL/catch-up failures.
//!
//! The model is pure: no clock, no network, no engine. Health, watermarks,
//! catch-up evidence, capacity, and load are injected through the public
//! [`ClusterSignals`]/[`PlacementSignals`] traits, so every assertion is
//! deterministic.

use std::collections::HashMap;
use std::time::Duration;

use reddb_server::cluster::{
    admit_durable_write, force_transition, plan_drain, run_drain, AdmissionOutcome,
    CatchUpEvidence, ClusterId, ClusterMember, ClusterSignals, ClusterSupervisor, CollectionId,
    CommitWatermark, DurableWriteReject, FenceReason, ForceTransitionCapability,
    ForcedTransitionDisposition, ForcedTransitionRequest, HealthPolicy, HintOutcome, JoinRequest,
    KeyTarget, LeasedOwner, MemberCapacity, MemberKind, MemberSignals, MembershipCatalog,
    NodeIdentity, OperatorReason, OwnerWriteMode, OwnershipEpoch, OwnershipLease,
    PlacementMetadata, PlacementPolicy, PlacementSignals, RangeBound, RangeBounds, RangeId,
    RangeLoad, RangeOwnership, RangeRequest, RangeRole, RangeWriteReject, RedirectReason,
    RequestOperation, RouteDecision, RoutedRequest, RoutingPolicy, SeedAuthority, ShardKeyMode,
    ShardOwnershipCatalog, SupervisorTerm, TransitionKind, WeightedPlacementPlanner,
    WriteTransactionReject,
};

// --- shared cluster vocabulary ------------------------------------------------

const CLUSTER: &str = "drill-cluster";
const NODE_A: &str = "CN=data-a,O=reddb";
const NODE_B: &str = "CN=data-b,O=reddb";
const NODE_C: &str = "CN=data-c,O=reddb";

fn ident(cn: &str) -> NodeIdentity {
    NodeIdentity::from_certificate_subject(cn).expect("valid certificate subject")
}

fn cluster_id() -> ClusterId {
    ClusterId::new(CLUSTER).expect("valid cluster id")
}

fn coll(name: &str) -> CollectionId {
    CollectionId::new(name).expect("valid collection id")
}

/// Lower half of the keyspace `[Min, 0x80)`.
fn lower_half() -> RangeBounds {
    RangeBounds::new(RangeBound::Min, RangeBound::key([0x80])).expect("valid bounds")
}

/// Upper half of the keyspace `[0x80, Max)`.
fn upper_half() -> RangeBounds {
    RangeBounds::new(RangeBound::key([0x80]), RangeBound::Max).expect("valid bounds")
}

/// A key that routes into the lower-half range.
const KEY_LOW: [u8; 1] = [0x10];
/// A key that routes into the upper-half range.
const KEY_HIGH: [u8; 1] = [0x90];

// --- injected, scriptable signals --------------------------------------------

/// One scriptable signal source standing in for the live cluster state the
/// control plane would read: per-member health, per-range commit watermarks,
/// per-(range, candidate) catch-up evidence, and per-member capacity / per-range
/// load for the rebalancer. Mutating it between scan steps is how the drill
/// "moves time forward" (a member fails, a replica catches up) without a clock.
struct DrillSignals {
    members: HashMap<NodeIdentity, MemberSignals>,
    watermarks: HashMap<(CollectionId, RangeId), CommitWatermark>,
    catch_up: HashMap<(CollectionId, RangeId, NodeIdentity), CatchUpEvidence>,
    capacity: HashMap<NodeIdentity, MemberCapacity>,
    load: HashMap<(CollectionId, RangeId), RangeLoad>,
}

impl DrillSignals {
    fn new() -> Self {
        Self {
            members: HashMap::new(),
            watermarks: HashMap::new(),
            catch_up: HashMap::new(),
            capacity: HashMap::new(),
            load: HashMap::new(),
        }
    }

    fn set_health(&mut self, cn: &str, signals: MemberSignals) {
        self.members.insert(ident(cn), signals);
    }

    fn set_watermark(
        &mut self,
        collection: &CollectionId,
        range_id: RangeId,
        watermark: CommitWatermark,
    ) {
        self.watermarks
            .insert((collection.clone(), range_id), watermark);
    }

    fn set_catch_up(
        &mut self,
        collection: &CollectionId,
        range_id: RangeId,
        cn: &str,
        applied_term: u64,
        applied_lsn: u64,
    ) {
        self.catch_up.insert(
            (collection.clone(), range_id, ident(cn)),
            CatchUpEvidence::new(ident(cn), applied_term, applied_lsn),
        );
    }

    fn set_capacity(&mut self, cn: &str, capacity: MemberCapacity) {
        self.capacity.insert(ident(cn), capacity);
    }

    fn set_load(&mut self, collection: &CollectionId, range_id: RangeId, load: RangeLoad) {
        self.load.insert((collection.clone(), range_id), load);
    }
}

impl ClusterSignals for DrillSignals {
    fn member_signals(&self, member: &NodeIdentity) -> MemberSignals {
        self.members
            .get(member)
            .copied()
            .unwrap_or_else(MemberSignals::healthy)
    }

    fn commit_watermark(&self, collection: &CollectionId, range_id: RangeId) -> CommitWatermark {
        self.watermarks
            .get(&(collection.clone(), range_id))
            .copied()
            .unwrap_or_else(|| CommitWatermark::new(1, 0))
    }

    fn catch_up(
        &self,
        collection: &CollectionId,
        range_id: RangeId,
        candidate: &NodeIdentity,
    ) -> Option<CatchUpEvidence> {
        self.catch_up
            .get(&(collection.clone(), range_id, candidate.clone()))
            .cloned()
    }
}

impl PlacementSignals for DrillSignals {
    fn member_capacity(&self, member: &NodeIdentity) -> MemberCapacity {
        self.capacity
            .get(member)
            .copied()
            .unwrap_or_else(|| MemberCapacity::with_disk(1_000_000_000))
    }

    fn range_load(&self, collection: &CollectionId, range_id: RangeId) -> RangeLoad {
        self.load
            .get(&(collection.clone(), range_id))
            .copied()
            .unwrap_or_else(|| RangeLoad::idle(0))
    }
}

/// Signals for a failed-and-past-grace owner: no heartbeat for a long time,
/// well over the default grace period, with high lag and errors.
fn failed_health() -> MemberSignals {
    MemberSignals {
        since_last_heartbeat: Duration::from_secs(60),
        replication_lag_lsn: 50_000,
        recent_errors: 100,
        unhealthy_for: Duration::from_secs(60),
    }
}

// --- shared cluster builders --------------------------------------------------

/// Drive a real join: node-a founds the cluster; node-b and node-c join through
/// the [`SeedAuthority`] against an allowlist. Returns the resulting authorised
/// 3-data-member membership catalog.
fn join_three_member_cluster() -> MembershipCatalog {
    let founders = [ClusterMember::joined_empty(ident(NODE_A), MemberKind::Data)];
    let catalog = MembershipCatalog::new(cluster_id(), founders);
    let mut seed = SeedAuthority::new(
        catalog,
        [
            (ident(NODE_B), MemberKind::Data),
            (ident(NODE_C), MemberKind::Data),
        ],
    );

    for cn in [NODE_B, NODE_C] {
        let grant = seed
            .evaluate_join(JoinRequest::authenticated(
                cluster_id(),
                ident(cn),
                MemberKind::Data,
            ))
            .unwrap_or_else(|rej| panic!("authorised join for {cn} should succeed: {rej:?}"));
        assert_eq!(grant.outcome, AdmissionOutcome::Admitted, "{cn} admitted");
    }

    seed.catalog().clone()
}

/// One range over `bounds`, owned by `owner` with `replicas`, at the initial
/// epoch/version.
fn range(
    collection: &CollectionId,
    id: u64,
    bounds: RangeBounds,
    owner: &str,
    replicas: &[&str],
) -> RangeOwnership {
    RangeOwnership::establish(
        collection.clone(),
        RangeId::new(id),
        ShardKeyMode::Ordered,
        bounds,
        ident(owner),
        replicas.iter().map(|r| ident(r)).collect::<Vec<_>>(),
        PlacementMetadata::with_replication_factor(3),
    )
}

/// The drill's standard placement: `orders` split into a lower-half range owned
/// by node-a (replicas b, c) and an upper-half range owned by node-b
/// (replicas a, c) — two ranges, two distinct owners, one keyspace.
fn ownership_two_ranges() -> (ShardOwnershipCatalog, CollectionId) {
    let orders = coll("orders");
    let mut catalog = ShardOwnershipCatalog::new();
    catalog
        .declare_collection(orders.clone(), ShardKeyMode::Ordered)
        .expect("declare orders");
    catalog
        .apply_update(range(&orders, 1, lower_half(), NODE_A, &[NODE_B, NODE_C]))
        .expect("range 1 applied");
    catalog
        .apply_update(range(&orders, 2, upper_half(), NODE_B, &[NODE_A, NODE_C]))
        .expect("range 2 applied");
    (catalog, orders)
}

// --- criterion 1: join, advertise topology, create ranges, any-node routing ---

#[test]
fn three_member_cluster_joins_advertises_topology_and_accepts_routed_writes() {
    // Join: a founds, b and c join through the seed authority.
    let membership = join_three_member_cluster();
    assert_eq!(
        membership.data_member_count(),
        3,
        "three data members after join"
    );
    for cn in [NODE_A, NODE_B, NODE_C] {
        assert!(
            membership.is_authorized(&ident(cn)),
            "{cn} is an authorised member"
        );
    }

    // Create ranges and advertise the topology a client would consume.
    let (catalog, orders) = ownership_two_ranges();
    let snapshot = catalog.topology_snapshot();
    assert_eq!(snapshot.ranges().len(), 2, "two ranges advertised");
    let low = snapshot.route(&orders, &KEY_LOW).expect("low key routes");
    let high = snapshot.route(&orders, &KEY_HIGH).expect("high key routes");
    assert_eq!(low.owner(), &ident(NODE_A), "advertised owner of low range");
    assert_eq!(
        high.owner(),
        &ident(NODE_B),
        "advertised owner of high range"
    );

    // Any-node routing: a write for the low-range key that lands on node-b (not
    // the owner) is routed to node-a rather than served locally.
    let policy = RoutingPolicy::forwarding();
    let write = RoutedRequest::new(
        orders.clone(),
        KEY_LOW.to_vec(),
        RequestOperation::Transaction,
    );
    match catalog.plan_route(&ident(NODE_B), &write, &policy) {
        RouteDecision::Redirect { hint, reason } => {
            assert_eq!(hint.owner(), &ident(NODE_A), "redirected to the owner");
            assert_eq!(
                reason,
                RedirectReason::Transaction,
                "txn writes redirect, never forward"
            );
        }
        other => panic!("expected redirect to owner, got {other:?}"),
    }

    // At the owner the same write routes Local and is admitted at the live epoch.
    let at_owner = catalog.plan_route(&ident(NODE_A), &write, &policy);
    assert!(at_owner.is_local(), "the owner serves the write locally");
    let admitted = catalog
        .admit_public_write(&ident(NODE_A), &orders, &KEY_LOW, OwnershipEpoch::initial())
        .expect("owner admits a write at the current epoch");
    assert_eq!(admitted.owner(), &ident(NODE_A));
}

// --- criterion 2: per-range owners, single-writer authority -------------------

#[test]
fn distinct_range_owners_each_hold_exclusive_single_writer_authority() {
    let (catalog, orders) = ownership_two_ranges();

    // Two ranges, two different owners.
    assert_eq!(
        catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
        &ident(NODE_A)
    );
    assert_eq!(
        catalog.range(&orders, RangeId::new(2)).unwrap().owner(),
        &ident(NODE_B)
    );

    // Roles are exclusive per range: A owns range 1 and is only a replica of 2.
    assert_eq!(
        catalog.role_at(&ident(NODE_A), &orders, RangeId::new(1)),
        Some(RangeRole::Owner)
    );
    assert_eq!(
        catalog.role_at(&ident(NODE_A), &orders, RangeId::new(2)),
        Some(RangeRole::Replica)
    );
    assert_eq!(
        catalog.role_at(&ident(NODE_B), &orders, RangeId::new(1)),
        Some(RangeRole::Replica)
    );
    assert_eq!(
        catalog.role_at(&ident(NODE_B), &orders, RangeId::new(2)),
        Some(RangeRole::Owner)
    );

    // The owner of a range admits writes to it...
    assert!(catalog
        .admit_public_write(&ident(NODE_A), &orders, &KEY_LOW, OwnershipEpoch::initial())
        .is_ok());
    assert!(catalog
        .admit_public_write(
            &ident(NODE_B),
            &orders,
            &KEY_HIGH,
            OwnershipEpoch::initial()
        )
        .is_ok());

    // ...but a replica may not write to a range it does not own — single-writer
    // authority holds even though both nodes carry both ranges.
    match catalog.admit_public_write(&ident(NODE_B), &orders, &KEY_LOW, OwnershipEpoch::initial()) {
        Err(RangeWriteReject::NotOwner { role, owner, .. }) => {
            assert_eq!(role, RangeRole::Replica);
            assert_eq!(owner, ident(NODE_A));
        }
        other => panic!("expected NotOwner rejection for a non-owner writer, got {other:?}"),
    }
    match catalog.admit_public_write(
        &ident(NODE_A),
        &orders,
        &KEY_HIGH,
        OwnershipEpoch::initial(),
    ) {
        Err(RangeWriteReject::NotOwner { owner, .. }) => assert_eq!(owner, ident(NODE_B)),
        other => panic!("expected NotOwner rejection, got {other:?}"),
    }
}

// --- criterion 3: stale client gets correction or safe forwarding -------------

#[test]
fn stale_client_is_corrected_by_redirect_and_safe_reads_are_forwarded() {
    let (mut catalog, orders) = ownership_two_ranges();
    // A client caches the topology, then ownership of the low range moves a -> b.
    let mut client =
        reddb_server::cluster::ClientTopology::from_snapshot(catalog.topology_snapshot());
    let current = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
    catalog
        .apply_update(current.transfer_to(ident(NODE_B), [ident(NODE_A), ident(NODE_C)]))
        .expect("ownership transfer applied");

    // The stale client still resolves the low key to the old owner (node-a).
    let stale_owner = client
        .resolve(&orders, &KEY_LOW)
        .expect("stale resolve")
        .clone();
    assert_eq!(stale_owner, ident(NODE_A), "client cache is stale");

    // A transaction sent to the stale owner is redirected; the hint names the
    // true owner and bumped epoch, and applying it corrects the cache.
    let txn = RoutedRequest::new(
        orders.clone(),
        KEY_LOW.to_vec(),
        RequestOperation::Transaction,
    );
    let hint = match catalog.plan_route(&stale_owner, &txn, &RoutingPolicy::forwarding()) {
        RouteDecision::Redirect { hint, .. } => hint,
        other => panic!("expected redirect for stale txn, got {other:?}"),
    };
    assert_eq!(hint.owner(), &ident(NODE_B), "redirect names the new owner");
    assert!(
        hint.epoch().value() > OwnershipEpoch::initial().value(),
        "epoch advanced on transfer"
    );
    assert_eq!(client.apply_hint(&hint), HintOutcome::Corrected);
    assert_eq!(
        client.resolve(&orders, &KEY_LOW).unwrap(),
        &ident(NODE_B),
        "cache corrected"
    );
    assert!(client.needs_refresh(), "a hint flags the cache as advisory");

    // Safe forwarding: a small safe-point read that lands on a non-owner is
    // forwarded to the owner rather than redirected back to the client.
    let read = RoutedRequest::new(
        orders.clone(),
        KEY_LOW.to_vec(),
        RequestOperation::SafePointOp,
    )
    .with_payload_len(64);
    match catalog.plan_route(&ident(NODE_A), &read, &RoutingPolicy::forwarding()) {
        RouteDecision::Forward { hint } => assert_eq!(hint.owner(), &ident(NODE_B)),
        other => panic!("expected safe forward to owner, got {other:?}"),
    }
}

#[test]
fn stale_topology_targeting_old_owner_cannot_create_split_brain_writes() {
    let (mut catalog, orders) = ownership_two_ranges();
    let mut client =
        reddb_server::cluster::ClientTopology::from_snapshot(catalog.topology_snapshot());
    let term = SupervisorTerm::genesis();

    let before = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
    let old_epoch = before.epoch();
    let old_owner_lease = OwnershipLease::grant(
        term,
        orders.clone(),
        RangeId::new(1),
        ident(NODE_A),
        old_epoch,
        0,
        10_000,
    );
    let old_owner = LeasedOwner::with_lease(old_owner_lease);

    catalog
        .apply_update(before.transfer_to(ident(NODE_B), [ident(NODE_A), ident(NODE_C)]))
        .expect("ownership transition applied");
    let after = catalog.range(&orders, RangeId::new(1)).unwrap();
    let new_epoch = after.epoch();
    assert!(
        new_epoch.value() > old_epoch.value(),
        "ownership transition bumped the fencing epoch"
    );

    // The cached topology is stale and still targets the old owner.
    let stale_owner = client
        .resolve(&orders, &KEY_LOW)
        .expect("stale topology resolves low key")
        .clone();
    assert_eq!(
        stale_owner,
        ident(NODE_A),
        "stale topology targets old owner"
    );

    // Routing through the live cluster seam returns a refreshable correction.
    let write = RoutedRequest::new(
        orders.clone(),
        KEY_LOW.to_vec(),
        RequestOperation::Transaction,
    );
    let hint = match catalog.plan_route(&stale_owner, &write, &RoutingPolicy::forwarding()) {
        RouteDecision::Redirect { hint, reason } => {
            assert_eq!(reason, RedirectReason::Transaction);
            hint
        }
        other => panic!("expected stale topology to redirect, got {other:?}"),
    };
    assert_eq!(hint.owner(), &ident(NODE_B));
    assert_eq!(hint.epoch(), new_epoch);
    assert_eq!(client.apply_hint(&hint), HintOutcome::Corrected);
    assert!(client.needs_refresh(), "hint is enough to trigger refresh");

    // Even if the stale topology sends the write to the old owner, the old
    // owner's lease is behind the current epoch and fences durable writes.
    match old_owner.admit_request(RangeRequest::DurableWrite, term, new_epoch, 500) {
        Err(err) => match err.reason {
            FenceReason::EpochSuperseded {
                lease_epoch,
                current_epoch,
            } => {
                assert_eq!(lease_epoch, old_epoch);
                assert_eq!(current_epoch, new_epoch);
            }
            other => panic!("expected epoch fence, got {other:?}"),
        },
        Ok(()) => panic!("old owner accepted a durable write after epoch bump"),
    }

    // The combined durable-write gate also rejects the old owner and carries the
    // current owner/epoch/version facts needed for safe retry.
    match admit_durable_write(
        &catalog,
        &old_owner,
        &stale_owner,
        &orders,
        &KEY_LOW,
        term,
        500,
    ) {
        Err(DurableWriteReject::StaleOwnership {
            attempted_owner,
            current_owner,
            attempted_epoch,
            current_epoch,
            ..
        }) => {
            assert_eq!(attempted_owner, stale_owner);
            assert_eq!(current_owner, ident(NODE_B));
            assert_eq!(attempted_epoch, old_epoch);
            assert_eq!(current_epoch, new_epoch);
        }
        other => panic!("expected stale owner durable-write rejection, got {other:?}"),
    }

    // The new owner, holding a lease for the bumped epoch, is the only node that
    // can accept the durable write.
    let new_owner = LeasedOwner::with_lease(OwnershipLease::grant(
        term,
        orders.clone(),
        RangeId::new(1),
        ident(NODE_B),
        new_epoch,
        0,
        10_000,
    ));
    let admitted = admit_durable_write(
        &catalog,
        &new_owner,
        &ident(NODE_B),
        &orders,
        &KEY_LOW,
        term,
        500,
    )
    .expect("new owner accepts durable write at the bumped epoch");
    assert_eq!(admitted.owner(), &ident(NODE_B));
    assert_eq!(admitted.epoch(), new_epoch);
}

// --- criterion 4: failover only when commit watermark is covered --------------

#[test]
fn failover_promotes_a_covered_replica_and_blocks_an_uncovered_one() {
    let orders = coll("orders");
    let supervisor = ClusterSupervisor::new(HealthPolicy::default());

    // The range's commit watermark the candidate must cover.
    let watermark = CommitWatermark::new(3, 1_000);

    // Scenario A: owner a fails; replica b has caught up past the watermark,
    // replica c has not. The supervisor must promote b, never c.
    {
        let membership = join_three_member_cluster();
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .declare_collection(orders.clone(), ShardKeyMode::Ordered)
            .unwrap();
        catalog
            .apply_update(range(
                &orders,
                1,
                RangeBounds::full(),
                NODE_A,
                &[NODE_B, NODE_C],
            ))
            .unwrap();

        let mut signals = DrillSignals::new();
        signals.set_health(NODE_A, failed_health());
        signals.set_watermark(&orders, RangeId::new(1), watermark);
        signals.set_catch_up(&orders, RangeId::new(1), NODE_B, 3, 1_000); // covers
        signals.set_catch_up(&orders, RangeId::new(1), NODE_C, 2, 200); // behind

        let (outcomes, plan) = supervisor.run_failovers(&membership, &mut catalog, &signals);
        assert_eq!(plan.promotions.len(), 1, "exactly one safe promotion");
        let promotion = &plan.promotions[0];
        assert_eq!(
            promotion.candidate,
            ident(NODE_B),
            "the covered replica is chosen, not c"
        );
        assert!(
            plan.blocked.is_empty(),
            "a covered candidate exists, nothing blocked"
        );

        let outcome = outcomes[0].as_ref().expect("promotion activates");
        assert_eq!(outcome.kind, TransitionKind::Promote);
        assert_eq!(outcome.new_owner, ident(NODE_B));
        assert!(
            outcome.fenced_old_owner(),
            "promotion fences the failed owner via the epoch bump"
        );

        // The catalog now reflects b as the single writer at the new epoch.
        let now = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(now.owner(), &ident(NODE_B));
        assert!(now.epoch().value() > OwnershipEpoch::initial().value());
    }

    // Scenario B: owner a fails but the only replica (c) is behind the
    // watermark. Promoting it could lose committed writes, so the supervisor
    // refuses and surfaces the range as blocked rather than failing over unsafely.
    {
        let membership = join_three_member_cluster();
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .declare_collection(orders.clone(), ShardKeyMode::Ordered)
            .unwrap();
        catalog
            .apply_update(range(&orders, 1, RangeBounds::full(), NODE_A, &[NODE_C]))
            .unwrap();

        let mut signals = DrillSignals::new();
        signals.set_health(NODE_A, failed_health());
        signals.set_watermark(&orders, RangeId::new(1), watermark);
        signals.set_catch_up(&orders, RangeId::new(1), NODE_C, 2, 200); // behind

        let plan = supervisor.plan_failovers(&membership, &catalog, &signals);
        assert!(
            plan.promotions.is_empty(),
            "no safe promotion when no replica covers the watermark"
        );
        assert_eq!(
            plan.blocked.len(),
            1,
            "the failing range is surfaced as blocked"
        );
        assert_eq!(
            plan.blocked[0].reason,
            reddb_server::cluster::BlockedReason::NoSafeCandidate
        );
        // Authority is untouched: a stays the owner, no silent data loss.
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident(NODE_A)
        );
    }
}

// --- criterion 5: forced recovery fences old owner + emits audit evidence -----

#[test]
fn forced_recovery_fences_the_old_owner_and_emits_audit_evidence() {
    let (mut catalog, orders) = ownership_two_ranges();
    let operator = ident("CN=operator,O=reddb");

    // An unauthorised force (no capability) is denied and the catalog untouched,
    // but it still produces an audit record — the evidence trail covers refusals.
    let denied = force_transition(
        &mut catalog,
        &ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident(NODE_C)),
        1_000,
    );
    assert!(!denied.is_allowed(), "missing capability is denied");
    assert!(
        matches!(denied.disposition(), ForcedTransitionDisposition::Denied(_)),
        "denial is recorded as audit evidence"
    );
    assert_eq!(
        catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
        &ident(NODE_A)
    );

    // An authorised force (capability + operator reason) recovers range 1 onto
    // node-c, fencing the old owner a.
    let request = ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident(NODE_C))
        .with_capability(ForceTransitionCapability::granted_to(operator.clone()))
        .with_reason(OperatorReason::new("node-a unreachable; manual recovery").unwrap())
        .with_replicas([ident(NODE_B)]);
    let audit = force_transition(&mut catalog, &request, 2_000);

    assert!(audit.is_allowed(), "authorised force is applied");
    assert!(
        audit.fenced_old_owner(),
        "the forced transition fences the old owner"
    );
    assert_eq!(
        audit.operator(),
        Some(&operator),
        "audit names the operator"
    );
    assert_eq!(
        audit.reason(),
        Some("node-a unreachable; manual recovery"),
        "audit carries the reason"
    );
    assert_eq!(audit.attempted_at_ms(), 2_000);
    let (prev_epoch, new_epoch) = match audit.disposition() {
        ForcedTransitionDisposition::Allowed {
            previous_owner,
            new_owner,
            previous_epoch,
            new_epoch,
            ..
        } => {
            assert_eq!(previous_owner, &ident(NODE_A));
            assert_eq!(new_owner, &ident(NODE_C));
            (*previous_epoch, *new_epoch)
        }
        other => panic!("expected an Allowed disposition, got {other:?}"),
    };
    assert!(
        new_epoch.value() > prev_epoch.value(),
        "epoch bumped — the fence"
    );

    // The fence is real: the old owner a, still believing it holds the prior
    // epoch, is rejected with a stale epoch when it tries to write.
    match catalog.admit_public_write(&ident(NODE_A), &orders, &KEY_LOW, prev_epoch) {
        Err(RangeWriteReject::NotOwner { owner, .. }) => assert_eq!(owner, ident(NODE_C)),
        Err(RangeWriteReject::StaleEpoch { .. }) => {}
        other => panic!("expected the fenced old owner to be rejected, got {other:?}"),
    }
    // The new owner c writes at the new epoch.
    assert!(catalog
        .admit_public_write(&ident(NODE_C), &orders, &KEY_LOW, new_epoch)
        .is_ok());
}

// --- criterion 6: drain + rebalance preserve write authority ------------------

#[test]
fn drain_and_rebalance_move_ownership_without_breaking_write_authority() {
    // Drain node-b: its owned range must hand off to a safe replica, and the
    // resulting catalog must still have exactly one owner per range. Range 1 is
    // owned by a (replica c only) and range 2 by b (replicas a, c); b owns one
    // range and replicates none, so a clean evacuation is possible in a
    // 3-member cluster (no replica slot needs a non-existent 4th replacement).
    let mut membership = join_three_member_cluster();
    let orders = coll("orders");
    let mut catalog = ShardOwnershipCatalog::new();
    catalog
        .declare_collection(orders.clone(), ShardKeyMode::Ordered)
        .expect("declare orders");
    catalog
        .apply_update(range(&orders, 1, lower_half(), NODE_A, &[NODE_C]))
        .expect("range 1 applied");
    catalog
        .apply_update(range(&orders, 2, upper_half(), NODE_B, &[NODE_A, NODE_C]))
        .expect("range 2 applied");

    let mut signals = DrillSignals::new();
    // All members healthy; the handoff targets are caught up to the watermark so
    // the handoff is safe.
    signals.set_watermark(&orders, RangeId::new(2), CommitWatermark::new(1, 500));
    signals.set_catch_up(&orders, RangeId::new(2), NODE_A, 1, 500);
    signals.set_catch_up(&orders, RangeId::new(2), NODE_C, 1, 500);

    assert_eq!(
        membership.begin_drain(&ident(NODE_B)),
        Some(true),
        "b enters draining"
    );
    let plan = plan_drain(&ident(NODE_B), &membership, &catalog, &signals);
    assert!(!plan.is_empty(), "draining b yields work — it owns range 2");
    assert!(
        plan.steps
            .iter()
            .any(|s| matches!(s, reddb_server::cluster::DrainStep::Handoff(_))),
        "the owned range is handed off, not dropped"
    );

    let outcome = run_drain(&ident(NODE_B), &membership, &mut catalog, &signals);
    assert!(outcome.is_drained(), "b's ranges are fully evacuated");

    // Single-writer authority survives the drain: range 2 has exactly one owner,
    // and it is no longer the drained node b.
    let range2 = catalog.range(&orders, RangeId::new(2)).unwrap();
    assert_ne!(
        range2.owner(),
        &ident(NODE_B),
        "the drained node no longer owns the range"
    );
    assert!(
        range2.epoch().value() > OwnershipEpoch::initial().value(),
        "handoff bumped the epoch"
    );
    // No range anywhere is still owned by the drained node.
    assert!(
        catalog.entries().all(|r| r.owner() != &ident(NODE_B)),
        "drained node owns nothing"
    );

    // Rebalance: on a fresh, all-active cluster, a hot, oversized range on a
    // capacity-starved owner must produce a move that respects ownership (it
    // moves *from* the current owner) and never invents a second writer.
    let membership = join_three_member_cluster();
    let (catalog, orders) = ownership_two_ranges();
    let mut signals = DrillSignals::new();
    // node-a is starved for capacity and its range is hot; node-c is roomy.
    signals.set_capacity(NODE_A, MemberCapacity::new(10_000_000, 1));
    signals.set_capacity(NODE_B, MemberCapacity::new(10_000_000, 1));
    signals.set_capacity(NODE_C, MemberCapacity::new(10_000_000_000, 1));
    signals.set_load(
        &orders,
        RangeId::new(1),
        RangeLoad {
            bytes_used: 9_000_000,
            read_ops: 100_000,
            write_ops: 100_000,
        },
    );
    signals.set_load(&orders, RangeId::new(2), RangeLoad::idle(1_000));

    let planner = WeightedPlacementPlanner::new(PlacementPolicy::default());
    let rebalance = planner.plan_rebalance(&membership, &catalog, &signals);
    assert!(
        !rebalance.is_empty(),
        "a skewed cluster yields a rebalance plan"
    );
    for mv in &rebalance.moves {
        let owner = catalog.range(&mv.collection, mv.range_id).unwrap().owner();
        assert_eq!(
            &mv.from, owner,
            "a planned move starts from the range's real current owner"
        );
        assert_ne!(
            mv.from, mv.to,
            "a move changes the owner — never a no-op rewrite"
        );
    }
}

// --- criterion 7: cross-range write rejected, read fanout works ---------------

#[test]
fn cross_range_writes_are_rejected_and_read_fanout_spans_owners() {
    let (catalog, orders) = ownership_two_ranges();

    // A write transaction touching one writer's range(s) is admissible.
    let single = catalog
        .plan_write_transaction(&[KeyTarget::new(orders.clone(), KEY_LOW.to_vec())])
        .expect("a single-writer transaction is plannable");
    assert_eq!(single.writer(), &ident(NODE_A));

    // A write transaction spanning two different owners' ranges is rejected:
    // this cut has no atomic cross-writer commit.
    match catalog.plan_write_transaction(&[
        KeyTarget::new(orders.clone(), KEY_LOW.to_vec()),
        KeyTarget::new(orders.clone(), KEY_HIGH.to_vec()),
    ]) {
        Err(WriteTransactionReject::CrossRange { writers }) => {
            assert_eq!(writers.len(), 2, "both writers are named");
            let named: Vec<_> = writers.iter().map(|w| w.writer().clone()).collect();
            assert!(named.contains(&ident(NODE_A)) && named.contains(&ident(NODE_B)));
        }
        other => panic!("expected a CrossRange rejection, got {other:?}"),
    }

    // A best-effort cross-range read fanout over the same two keys succeeds,
    // with one leg per owner.
    let fanout = catalog
        .plan_read_fanout(
            &[
                KeyTarget::new(orders.clone(), KEY_LOW.to_vec()),
                KeyTarget::new(orders.clone(), KEY_HIGH.to_vec()),
            ],
            reddb_server::cluster::ReadFanoutPolicy::explicit(
                reddb_server::cluster::ReadFanoutBudget::default(),
            ),
        )
        .expect("explicit cross-range read fanout is plannable");
    assert!(
        fanout.is_cross_range(),
        "the read spans more than one owner"
    );
    assert_eq!(fanout.legs().len(), 2, "one leg per range owner");
    assert_eq!(fanout.trace().owner_count(), 2);
    assert_eq!(fanout.trace().range_count(), 2);
    let leg_owners: Vec<_> = fanout.legs().iter().map(|l| l.owner().clone()).collect();
    assert!(leg_owners.contains(&ident(NODE_A)) && leg_owners.contains(&ident(NODE_B)));
}

// --- criterion 8: the whole drill records debuggable diagnostics --------------

/// A structured diagnostics ledger the drill writes as it advances. The point of
/// the criterion is operability: after the drill, the ledger must carry enough
/// to reconstruct *why* each membership / ownership-epoch / lease / catch-up
/// decision went the way it did.
#[derive(Default)]
struct DrillDiagnostics {
    lines: Vec<String>,
}

impl DrillDiagnostics {
    fn record(&mut self, dimension: &str, detail: impl Into<String>) {
        self.lines.push(format!("[{dimension}] {}", detail.into()));
    }

    fn dump(&self) -> String {
        self.lines.join("\n")
    }

    fn has(&self, dimension: &str) -> bool {
        self.lines
            .iter()
            .any(|l| l.starts_with(&format!("[{dimension}]")))
    }
}

#[test]
fn whole_drill_records_membership_epoch_lease_and_catchup_diagnostics() {
    let mut diag = DrillDiagnostics::new_default();

    // --- membership ---------------------------------------------------------
    let mut membership = join_three_member_cluster();
    diag.record(
        "membership",
        format!(
            "joined cluster={} data_members={}",
            membership.cluster_id().as_str(),
            membership.data_member_count()
        ),
    );

    // --- ownership epoch ----------------------------------------------------
    let (mut catalog, orders) = ownership_two_ranges();
    for entry in catalog.entries() {
        diag.record(
            "ownership-epoch",
            format!(
                "range={} owner={} epoch={} version={}",
                entry.range_id().value(),
                entry.owner().as_str(),
                entry.epoch().value(),
                entry.version().value(),
            ),
        );
    }

    // --- lease --------------------------------------------------------------
    // node-a holds a valid lease for range 1 and writes durably; once the lease
    // lapses it self-fences, which the ledger captures with the fence reason.
    let lease = OwnershipLease::grant(
        SupervisorTerm::genesis(),
        orders.clone(),
        RangeId::new(1),
        ident(NODE_A),
        OwnershipEpoch::initial(),
        0,
        1_000,
    );
    let holder = LeasedOwner::with_lease(lease);
    let live = holder.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 500);
    assert_eq!(live, OwnerWriteMode::Durable);
    diag.record("lease", "range=1 owner=data-a mode=Durable t=500ms");
    let admitted = admit_durable_write(
        &catalog,
        &holder,
        &ident(NODE_A),
        &orders,
        &KEY_LOW,
        SupervisorTerm::genesis(),
        500,
    );
    assert!(admitted.is_ok(), "leased owner admits a durable write");

    let lapsed = holder.evaluate(SupervisorTerm::genesis(), OwnershipEpoch::initial(), 1_500);
    match lapsed {
        OwnerWriteMode::Fenced(FenceReason::Expired {
            now_ms,
            expires_at_ms,
        }) => {
            diag.record(
                "lease",
                format!("range=1 self-fenced reason=Expired now={now_ms} expires={expires_at_ms}"),
            );
        }
        other => panic!("expected an expired self-fence, got {other:?}"),
    }
    // A durable write past lease expiry is fenced — the ledger explains why.
    match admit_durable_write(
        &catalog,
        &holder,
        &ident(NODE_A),
        &orders,
        &KEY_LOW,
        SupervisorTerm::genesis(),
        1_500,
    ) {
        Err(DurableWriteReject::Fenced { reason, .. }) => {
            diag.record(
                "lease",
                format!("range=1 durable-write fenced reason={reason:?}"),
            );
        }
        other => panic!("expected a fenced durable write past expiry, got {other:?}"),
    }

    // --- WAL / catch-up -----------------------------------------------------
    // Fail node-a and fail over range 1; the ledger records the commit watermark
    // and each candidate's catch-up position — exactly what is needed to debug a
    // WAL/catch-up failover.
    let watermark = CommitWatermark::new(3, 1_000);
    let mut signals = DrillSignals::new();
    signals.set_health(NODE_A, failed_health());
    signals.set_watermark(&orders, RangeId::new(1), watermark);
    signals.set_catch_up(&orders, RangeId::new(1), NODE_B, 3, 1_000); // covers
    signals.set_catch_up(&orders, RangeId::new(1), NODE_C, 2, 200); // behind
    diag.record(
        "wal-catchup",
        format!(
            "range=1 watermark=(term={},lsn={}) data-b=(3,1000)->covers data-c=(2,200)->behind",
            watermark.term, watermark.lsn
        ),
    );

    let supervisor = ClusterSupervisor::new(HealthPolicy::default());
    let (outcomes, plan) = supervisor.run_failovers(&membership, &mut catalog, &signals);
    let promotion = &plan.promotions[0];
    assert_eq!(promotion.candidate, ident(NODE_B));
    let outcome = outcomes[0].as_ref().expect("promotion activates");
    diag.record(
        "ownership-epoch",
        format!(
            "range=1 promoted owner={} epoch {}->{} (watermark term={})",
            outcome.new_owner.as_str(),
            outcome.previous_epoch.value(),
            outcome.new_epoch.value(),
            outcome.watermark.term,
        ),
    );

    // Keep the membership value used (and assert it stays a 3-member cluster) so
    // the diagnostics genuinely tie back to live state.
    assert_eq!(membership.len(), 3);
    let _ = membership.member_mut(&ident(NODE_A));

    // --- the criterion: all four debug dimensions are present and non-empty --
    for dimension in ["membership", "ownership-epoch", "lease", "wal-catchup"] {
        assert!(
            diag.has(dimension),
            "diagnostics must cover '{dimension}' to be debuggable; got:\n{}",
            diag.dump()
        );
    }
    // The epoch dimension must show an actual transition (a "->"), and the
    // catch-up dimension must record the watermark — the two failure modes the
    // criterion calls out by name.
    assert!(
        diag.dump().contains("epoch 1->"),
        "epoch transition is recorded:\n{}",
        diag.dump()
    );
    assert!(
        diag.dump().contains("watermark="),
        "commit watermark is recorded:\n{}",
        diag.dump()
    );
}

impl DrillDiagnostics {
    fn new_default() -> Self {
        DrillDiagnostics::default()
    }
}
