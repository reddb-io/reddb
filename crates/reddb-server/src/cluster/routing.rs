//! Any-node routing and stale-ownership responses (issue #993, PRD #987, ADR 0037).
//!
//! ADR 0037 makes any-node routing **mandatory**: a client may send a request to
//! *any* data member, and that member must do something correct even when it is
//! not the owner of the range the request targets. PRD #987 spells out the two
//! correct things a non-owner may do:
//!
//! * **Forward** a *simple, safe* operation internally to the range owner, so the
//!   client never has to learn the topology to make progress; or
//! * **Redirect** — return enough routing information (current owner, ownership
//!   epoch, catalog version) for the client/router to refresh its topology and
//!   retry against the owner itself.
//!
//! The split between the two is deliberate and is the heart of this module.
//! Hidden internal forwarding is only ever safe for operations whose semantics do
//! not depend on *where* they run: a single-key point read or write. Anything
//! whose correctness is bound to a session or a long-lived stream —
//! transactions, streaming/cursor operations, oversized payloads, or operations
//! the caller has explicitly marked unsafe — must **not** be silently relayed.
//! For those the only safe answer is an honest redirect so the client opens its
//! transaction / stream / large transfer directly against the owner. This mirrors
//! ADR 0037's "routing must consult ownership metadata with an epoch/version and
//! handle stale routing responses" — and crucially it never weakens the
//! fencing-below-routing guarantee, because a forwarded write still lands on the
//! owner's [`admit_public_write`](ShardOwnershipCatalog::admit_public_write) gate
//! (issue #990) at the owner's *current* epoch.
//!
//! Like the rest of the cluster module this is a pure decision layer with no I/O:
//! [`plan_route`](ShardOwnershipCatalog::plan_route) maps a
//! ([`RoutedRequest`], local [`NodeIdentity`], [`RoutingPolicy`]) triple to a
//! [`RouteDecision`], so the any-node routing contract is exercised
//! deterministically. The transport that actually forwards bytes or writes a
//! redirect onto the wire is a separate concern layered on top of this.

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogVersion, CollectionId, OwnershipEpoch, RangeId, RangeOwnership, RangeRole,
    ShardOwnershipCatalog,
};

/// Default ceiling on the payload a non-owner will relay internally.
///
/// Forwarding copies the whole payload across an extra internal hop, so a large
/// transfer is cheaper and clearer to redirect: the client sends it once,
/// directly to the owner. The MVP budget is 1 MiB; operators can widen or narrow
/// it per [`RoutingPolicy`].
pub const DEFAULT_MAX_FORWARD_PAYLOAD: usize = 1024 * 1024;

/// What a request asks the cluster to do, reduced to just what routing needs to
/// decide forward-vs-redirect.
///
/// Only [`SafePointOp`](Self::SafePointOp) is eligible for hidden internal
/// forwarding. The other classes are exactly PRD #987's "must not be silently
/// forwarded" set: their correctness is tied to running on the owner directly, so
/// a non-owner must redirect rather than relay them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOperation {
    /// A single-key point read or write — the one class safe to forward to the
    /// owner, because its result does not depend on which node relays it.
    SafePointOp,
    /// A multi-statement transaction. Atomicity and session state must be
    /// established on the owner directly; never hidden-forwarded.
    Transaction,
    /// A streaming / cursor operation (scan, subscribe, change feed). The stream
    /// must originate on the owner; never hidden-forwarded.
    Streaming,
    /// An operation the caller has explicitly flagged as unsafe to forward.
    ExplicitlyUnsafe,
}

impl RequestOperation {
    /// `Ok` if this class is *in principle* forwardable (only
    /// [`SafePointOp`](Self::SafePointOp)); otherwise the [`RedirectReason`] that
    /// explains why it must be redirected instead.
    fn forwardable(self) -> Result<(), RedirectReason> {
        match self {
            RequestOperation::SafePointOp => Ok(()),
            RequestOperation::Transaction => Err(RedirectReason::Transaction),
            RequestOperation::Streaming => Err(RedirectReason::Streaming),
            RequestOperation::ExplicitlyUnsafe => Err(RedirectReason::ExplicitlyUnsafe),
        }
    }
}

/// A request as it arrives at some data member, abstracted to what routing reads:
/// which collection/key it targets, what kind of operation it is, and how large
/// its payload is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedRequest {
    collection: CollectionId,
    key: Vec<u8>,
    operation: RequestOperation,
    payload_len: usize,
}

impl RoutedRequest {
    /// A request with no meaningful payload (e.g. a point read or delete).
    pub fn new(
        collection: CollectionId,
        key: impl Into<Vec<u8>>,
        operation: RequestOperation,
    ) -> Self {
        Self {
            collection,
            key: key.into(),
            operation,
            payload_len: 0,
        }
    }

    /// Declare the request's payload size, so the forward-size budget can apply.
    pub fn with_payload_len(mut self, payload_len: usize) -> Self {
        self.payload_len = payload_len;
        self
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn operation(&self) -> RequestOperation {
        self.operation
    }

    pub fn payload_len(&self) -> usize {
        self.payload_len
    }
}

/// How a data member handles requests it does not own.
///
/// "Any-node routing is mandatory" (ADR 0037), but *forwarding* is a policy
/// choice: a deployment (or a single node) may prefer to push routing work onto
/// topology-aware clients and redirect everything instead of relaying. When
/// forwarding is disabled, even a safe point op gets a redirect — this is PRD
/// #987's "when forwarding is not selected" path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingPolicy {
    forwarding_enabled: bool,
    max_forward_payload: usize,
}

impl RoutingPolicy {
    /// Forward safe point ops to the owner; redirect everything else. Uses the
    /// [`DEFAULT_MAX_FORWARD_PAYLOAD`] budget.
    pub fn forwarding() -> Self {
        Self {
            forwarding_enabled: true,
            max_forward_payload: DEFAULT_MAX_FORWARD_PAYLOAD,
        }
    }

    /// Never forward — always redirect a non-owner request with a routing hint.
    /// The client/router refreshes topology and retries against the owner.
    pub fn redirect_only() -> Self {
        Self {
            forwarding_enabled: false,
            max_forward_payload: 0,
        }
    }

    /// Override the maximum payload eligible for internal forwarding.
    pub fn with_max_forward_payload(mut self, max_forward_payload: usize) -> Self {
        self.max_forward_payload = max_forward_payload;
        self
    }

    pub fn forwarding_enabled(&self) -> bool {
        self.forwarding_enabled
    }

    pub fn max_forward_payload(&self) -> usize {
        self.max_forward_payload
    }
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self::forwarding()
    }
}

/// The routing information a non-owner hands back so the caller can reach the
/// owner — the payload of both a forward (where to relay) and a redirect (where
/// to retry).
///
/// It carries the owner's [`NodeIdentity`] plus the [`OwnershipEpoch`] and
/// [`CatalogVersion`] the decision was made at. The epoch/version let a
/// topology-aware client tell *stale* hints from fresh ones and avoid retry loops
/// against ownership that has since moved again (ADR 0037: routing "must consult
/// ownership metadata with an epoch/version").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingHint {
    collection: CollectionId,
    range_id: RangeId,
    owner: NodeIdentity,
    epoch: OwnershipEpoch,
    version: CatalogVersion,
}

impl RoutingHint {
    fn from_range(collection: &CollectionId, range: &RangeOwnership) -> Self {
        Self {
            collection: collection.clone(),
            range_id: range.range_id(),
            owner: range.owner().clone(),
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

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }

    pub fn version(&self) -> CatalogVersion {
        self.version
    }
}

impl std::fmt::Display for RoutingHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{} owned by {} at epoch {} (catalog version {})",
            self.collection, self.range_id, self.owner, self.epoch, self.version
        )
    }
}

/// Why a non-owner redirected a request instead of forwarding it.
///
/// Every redirect happens because the local node is not the owner; the reason
/// explains why the safe-forward path was *not* taken for this particular
/// request, so an operator (or a client deciding how to retry) can tell a routine
/// "open your transaction on the owner" from a "this node won't relay" policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectReason {
    /// This node's [`RoutingPolicy`] does not forward; the client must route to
    /// the owner itself. (PRD #987 "when forwarding is not selected".)
    ForwardingDisabled,
    /// A multi-statement transaction — must be opened on the owner directly.
    Transaction,
    /// A streaming / cursor operation — must originate on the owner.
    Streaming,
    /// The payload exceeds the forward budget; send it once, directly to the
    /// owner, rather than copying it across an extra internal hop.
    LargePayload { len: usize, limit: usize },
    /// The caller explicitly marked the operation unsafe to forward.
    ExplicitlyUnsafe,
}

impl std::fmt::Display for RedirectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForwardingDisabled => write!(f, "forwarding not selected on this node"),
            Self::Transaction => write!(f, "transactions must be opened on the owner"),
            Self::Streaming => write!(f, "streaming operations must originate on the owner"),
            Self::LargePayload { len, limit } => write!(
                f,
                "payload {len} bytes exceeds the {limit}-byte forward budget; send directly to the owner"
            ),
            Self::ExplicitlyUnsafe => {
                write!(f, "operation explicitly marked unsafe to forward")
            }
        }
    }
}

/// What a data member decides to do with a request under any-node routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// The local node owns the range — execute locally. Carries the range and the
    /// current ownership epoch the write should be stamped/fenced with (the same
    /// epoch [`admit_public_write`](ShardOwnershipCatalog::admit_public_write)
    /// will check).
    Local {
        range_id: RangeId,
        epoch: OwnershipEpoch,
    },
    /// A safe point op the local node may forward internally to the owner named
    /// in the hint. The forwarded write still passes the owner's public-write
    /// gate at the owner's current epoch, so forwarding never bypasses fencing.
    Forward { hint: RoutingHint },
    /// The request is not eligible for hidden forwarding (unsafe class, oversized
    /// payload, or forwarding disabled). Return the hint so the client refreshes
    /// topology and retries against the owner. This is the stale/misrouted
    /// response of acceptance criterion #2.
    Redirect {
        hint: RoutingHint,
        reason: RedirectReason,
    },
    /// No range of the collection covers the key. The catalog the request
    /// resolved against is empty or stale for this collection; the client must
    /// refresh its catalog and retry — there is no owner to name yet.
    Unroutable { collection: CollectionId },
}

impl RouteDecision {
    /// The routing hint, if this decision carries one (forward or redirect).
    pub fn hint(&self) -> Option<&RoutingHint> {
        match self {
            RouteDecision::Forward { hint } | RouteDecision::Redirect { hint, .. } => Some(hint),
            RouteDecision::Local { .. } | RouteDecision::Unroutable { .. } => None,
        }
    }

    /// Whether the local node should execute the request itself.
    pub fn is_local(&self) -> bool {
        matches!(self, RouteDecision::Local { .. })
    }
}

impl ShardOwnershipCatalog {
    /// Plan how `local` should handle `request` under `policy` — the any-node
    /// routing decision (issue #993).
    ///
    /// Resolves the target range from the catalog, then:
    ///
    /// * owner of the range → [`Local`](RouteDecision::Local);
    /// * non-owner, forwarding enabled, safe point op within budget →
    ///   [`Forward`](RouteDecision::Forward) to the owner;
    /// * non-owner but the op is unsafe to forward / oversized / forwarding
    ///   disabled → [`Redirect`](RouteDecision::Redirect) with the owner+epoch
    ///   hint;
    /// * no range covers the key → [`Unroutable`](RouteDecision::Unroutable).
    ///
    /// The decision is pure: it reads the catalog and returns intent. Fencing is
    /// still enforced below routing — a forwarded or locally-executed write lands
    /// on [`admit_public_write`](Self::admit_public_write) at the owner's current
    /// epoch, so a stale routing decision cannot smuggle a write past ownership.
    pub fn plan_route(
        &self,
        local: &NodeIdentity,
        request: &RoutedRequest,
        policy: &RoutingPolicy,
    ) -> RouteDecision {
        let range = match self.route(request.collection(), request.key()) {
            Some(range) => range,
            None => {
                return RouteDecision::Unroutable {
                    collection: request.collection().clone(),
                }
            }
        };

        if range.role_of(local) == RangeRole::Owner {
            return RouteDecision::Local {
                range_id: range.range_id(),
                epoch: range.epoch(),
            };
        }

        let hint = RoutingHint::from_range(request.collection(), range);

        // Non-owner. Forward only if policy allows AND the op is a safe point op
        // within the forward-size budget; otherwise hand back a routing hint.
        if !policy.forwarding_enabled() {
            return RouteDecision::Redirect {
                hint,
                reason: RedirectReason::ForwardingDisabled,
            };
        }
        if let Err(reason) = request.operation().forwardable() {
            return RouteDecision::Redirect { hint, reason };
        }
        if request.payload_len() > policy.max_forward_payload() {
            return RouteDecision::Redirect {
                hint,
                reason: RedirectReason::LargePayload {
                    len: request.payload_len(),
                    limit: policy.max_forward_payload(),
                },
            };
        }
        RouteDecision::Forward { hint }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{PlacementMetadata, RangeBound, RangeBounds, ShardKeyMode};

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    /// A full-keyspace range of `coll` owned by `owner` with `replicas`.
    fn range_with(coll: &CollectionId, id: u64, owner: &str, replicas: &[&str]) -> RangeOwnership {
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

    fn catalog_with(range: RangeOwnership) -> ShardOwnershipCatalog {
        let mut catalog = ShardOwnershipCatalog::new();
        catalog.apply_update(range).unwrap();
        catalog
    }

    // AC #1 + direct-owner request: the owner resolves the range and executes
    // locally.
    #[test]
    fn owner_executes_locally() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        let decision =
            catalog.plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding());
        assert_eq!(
            decision,
            RouteDecision::Local {
                range_id: RangeId::new(1),
                epoch: OwnershipEpoch::initial(),
            }
        );
        assert!(decision.is_local());
        assert!(decision.hint().is_none());
    }

    // AC #1: any node can resolve target range ownership from the catalog, even
    // one that holds no copy of the range.
    #[test]
    fn any_node_resolves_owner_from_catalog() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        // node-c holds no copy at all, yet can still name the owner.
        let decision =
            catalog.plan_route(&ident("CN=node-c"), &request, &RoutingPolicy::forwarding());
        let hint = decision.hint().expect("non-owner carries a hint");
        assert_eq!(hint.owner(), &ident("CN=node-a"));
        assert_eq!(hint.range_id(), RangeId::new(1));
        assert_eq!(hint.epoch(), OwnershipEpoch::initial());
    }

    // AC #3: a safe single-key op from a non-owner is forwarded to the owner.
    #[test]
    fn safe_point_op_is_forwarded_from_non_owner() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        // node-b is a replica → forward to the owner node-a.
        let decision =
            catalog.plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding());
        match decision {
            RouteDecision::Forward { hint } => {
                assert_eq!(hint.owner(), &ident("CN=node-a"));
                assert_eq!(hint.epoch(), OwnershipEpoch::initial());
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    // AC #3: a forwarded write does not bypass fencing — it still passes the
    // owner's public-write gate (#990) at the hint's epoch.
    #[test]
    fn forwarded_write_still_passes_owner_public_gate() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        let hint =
            match catalog.plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding()) {
                RouteDecision::Forward { hint } => hint,
                other => panic!("expected Forward, got {other:?}"),
            };
        // The owner admits the relayed write at the epoch the forwarder carried.
        let admitted = catalog
            .admit_public_write(&ident("CN=node-a"), &orders, b"k", hint.epoch())
            .expect("owner admits the forwarded write at the current epoch");
        assert_eq!(admitted.owner(), &ident("CN=node-a"));
    }

    // AC #4: transactions are redirected, never hidden-forwarded.
    #[test]
    fn transaction_from_non_owner_is_redirected() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);

        let decision =
            catalog.plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding());
        match decision {
            RouteDecision::Redirect { hint, reason } => {
                assert_eq!(reason, RedirectReason::Transaction);
                assert_eq!(hint.owner(), &ident("CN=node-a"));
            }
            other => panic!("expected Redirect(Transaction), got {other:?}"),
        }
    }

    // AC #4: streaming operations are redirected.
    #[test]
    fn streaming_from_non_owner_is_redirected() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Streaming);

        match catalog.plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding()) {
            RouteDecision::Redirect { reason, .. } => assert_eq!(reason, RedirectReason::Streaming),
            other => panic!("expected Redirect(Streaming), got {other:?}"),
        }
    }

    // AC #4: explicitly unsafe operations are redirected.
    #[test]
    fn explicitly_unsafe_op_is_redirected() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request = RoutedRequest::new(
            orders.clone(),
            b"k".to_vec(),
            RequestOperation::ExplicitlyUnsafe,
        );

        match catalog.plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding()) {
            RouteDecision::Redirect { reason, .. } => {
                assert_eq!(reason, RedirectReason::ExplicitlyUnsafe)
            }
            other => panic!("expected Redirect(ExplicitlyUnsafe), got {other:?}"),
        }
    }

    // AC #4: an over-budget payload is redirected even though its op class is
    // safe — send it once, directly to the owner.
    #[test]
    fn large_payload_is_redirected_not_forwarded() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let policy = RoutingPolicy::forwarding().with_max_forward_payload(64);
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp)
                .with_payload_len(65);

        match catalog.plan_route(&ident("CN=node-b"), &request, &policy) {
            RouteDecision::Redirect { reason, .. } => {
                assert_eq!(reason, RedirectReason::LargePayload { len: 65, limit: 64 })
            }
            other => panic!("expected Redirect(LargePayload), got {other:?}"),
        }
        // A payload exactly at the budget is still forwardable.
        let at_budget =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp)
                .with_payload_len(64);
        assert!(matches!(
            catalog.plan_route(&ident("CN=node-b"), &at_budget, &policy),
            RouteDecision::Forward { .. }
        ));
    }

    // AC #2 "when forwarding is not selected": redirect-only policy redirects even
    // a safe point op.
    #[test]
    fn redirect_only_policy_redirects_safe_op() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        match catalog.plan_route(
            &ident("CN=node-b"),
            &request,
            &RoutingPolicy::redirect_only(),
        ) {
            RouteDecision::Redirect { hint, reason } => {
                assert_eq!(reason, RedirectReason::ForwardingDisabled);
                assert_eq!(hint.owner(), &ident("CN=node-a"));
                assert_eq!(hint.epoch(), OwnershipEpoch::initial());
            }
            other => panic!("expected Redirect(ForwardingDisabled), got {other:?}"),
        }
    }

    // A key with no covering range is unroutable — refresh the catalog.
    #[test]
    fn key_with_no_range_is_unroutable() {
        let catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);

        let decision =
            catalog.plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding());
        assert_eq!(decision, RouteDecision::Unroutable { collection: orders });
        assert!(decision.hint().is_none());
    }

    // AC #5: stale ownership retry behavior. A client routes against an old
    // catalog snapshot, sends to the former owner, gets a redirect carrying the
    // new owner+epoch, refreshes, and retries successfully against the new owner.
    #[test]
    fn stale_ownership_redirects_then_retry_succeeds() {
        let orders = collection("orders");

        // v1: node-a owns the range; node-b is a replica.
        let mut catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));

        // Ownership transfers a → b (epoch + version bump). node-a becomes a
        // replica of the range it used to own.
        let v1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        let v2 = v1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]);
        catalog.apply_update(v2).unwrap();

        // A client with a stale snapshot still believes node-a is the owner and
        // sends a transaction there. node-a is now a replica → it redirects with
        // the *current* owner and the advanced epoch.
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
        let redirect =
            catalog.plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding());
        let hint = match redirect {
            RouteDecision::Redirect { hint, reason } => {
                assert_eq!(reason, RedirectReason::Transaction);
                hint
            }
            other => panic!("expected Redirect, got {other:?}"),
        };
        assert_eq!(hint.owner(), &ident("CN=node-b"));
        assert_eq!(hint.epoch().value(), 2);
        assert!(hint.epoch() > OwnershipEpoch::initial());

        // The client refreshes from the hint and retries against the new owner.
        let retry = catalog.plan_route(hint.owner(), &request, &RoutingPolicy::forwarding());
        assert_eq!(
            retry,
            RouteDecision::Local {
                range_id: RangeId::new(1),
                epoch: hint.epoch(),
            }
        );
    }

    // A stale safe-op forward also tracks ownership: after a transfer, a safe op
    // arriving at the old owner forwards to the *new* owner, not the old one.
    #[test]
    fn safe_op_forward_targets_current_owner_after_transfer() {
        let orders = collection("orders");
        let mut catalog = catalog_with(range_with(&orders, 1, "CN=node-a", &["CN=node-b"]));
        let v1 = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
        catalog
            .apply_update(v1.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
            .unwrap();

        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::SafePointOp);
        match catalog.plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding()) {
            RouteDecision::Forward { hint } => {
                assert_eq!(hint.owner(), &ident("CN=node-b"));
                assert_eq!(hint.epoch().value(), 2);
            }
            other => panic!("expected Forward to new owner, got {other:?}"),
        }
    }

    #[test]
    fn routing_hint_display_names_owner_and_epoch() {
        let orders = collection("orders");
        let catalog = catalog_with(range_with(&orders, 4, "CN=node-a", &["CN=node-b"]));
        let request =
            RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
        let hint = catalog
            .plan_route(&ident("CN=node-b"), &request, &RoutingPolicy::forwarding())
            .hint()
            .cloned()
            .expect("redirect carries a hint");
        let rendered = hint.to_string();
        assert!(rendered.contains("CN=node-a"));
        assert!(rendered.contains("epoch 1"));
    }
}
