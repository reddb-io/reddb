//! Forced ownership transitions for disaster recovery (issue #999, PRD #987,
//! ADR 0037).
//!
//! The [ordinary transition machine](super::ownership_transition) moves range
//! authority *safely*: it demands a three-part compare-and-swap on the current
//! owner/epoch/version and proof that the candidate's applied log covers the range
//! commit watermark. That is exactly right when the cluster is healthy — but a
//! disaster (the owner and a quorum of replicas are simultaneously lost) can leave
//! a range with *no* candidate that can satisfy those checks, and therefore no way
//! to recover through the ordinary path. This module is the deliberately
//! dangerous escape hatch the ADR reserves for that case.
//!
//! Per ADR 0037: *"Forced transitions are reserved for disaster recovery. Normal
//! ownership transitions require the ordinary cluster safety checks. A `FORCE`
//! transition may proceed without ordinary quorum only with a special
//! administrative capability, explicit operator reason, durable audit evidence,
//! and an ownership epoch bump that fences any old owner that later reappears."*
//! Each of those four conditions is a structural part of this module:
//!
//! 1. **Distinct administrative capability.** A forced transition is authorised by
//!    a [`ForceTransitionCapability`], a privilege *separate* from the authority to
//!    run an ordinary transition. The ordinary [`run_transition`] path never
//!    consults it, and this path refuses outright
//!    ([`ForceDenial::MissingCapability`]) when it is absent — there is no way to
//!    force a transition by accident.
//! 2. **Explicit operator reason.** The operator must attach a non-empty
//!    [`OperatorReason`]; a forced transition with no stated justification is
//!    refused ([`ForceDenial::MissingReason`]). The reason is recorded in the audit
//!    evidence so a later reviewer learns *why* quorum was bypassed.
//! 3. **Durable audit evidence for every attempt.** [`force_transition`] *always*
//!    returns a [`ForcedTransitionAudit`] — for allowed, denied, *and* failed
//!    attempts alike. A privileged operation that can bypass quorum must leave a
//!    trail whether or not it succeeded, so the evidence is the function's return
//!    value, not a side effect a caller can forget to capture.
//! 4. **Epoch bump that fences the old owner.** A successful force installs a new
//!    catalog entry via [`RangeOwnership::transfer_to`], which bumps the ownership
//!    epoch. From that instant the old owner's epoch is stale: should it reappear
//!    (the partition that "killed" it heals), [`admit_public_write`] rejects its
//!    writes exactly as it would after an ordinary transition.
//!
//! ## What force bypasses — and what it does not
//!
//! Force exists *because* ordinary safety cannot be met, so it skips the CAS, the
//! catch-up safety evidence, and the replica-membership check: the operator may
//! install **any** target node, even one the catalog does not list as a replica,
//! because in a true disaster the surviving copy may be exactly such a node. What
//! force does **not** skip is the catalog's own structural integrity: the range
//! must exist, and the epoch/version still advance monotonically through
//! [`apply_update`]. A force against an unknown range is a
//! [`ForceFailure::UnknownRange`], audited like any other failed attempt.
//!
//! Like its siblings this module is a pure, deterministic data model over the
//! catalog — time (`now_ms`) is passed in for the audit timestamp rather than read
//! from a clock — so the capability, reason, fencing, and audit story is exercised
//! without any I/O.
//!
//! [`run_transition`]: super::ownership_transition::run_transition
//! [`admit_public_write`]: super::ownership::ShardOwnershipCatalog::admit_public_write

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogError, CatalogVersion, CollectionId, OwnershipEpoch, RangeId, RangeOwnership,
    ShardOwnershipCatalog,
};

/// A distinct administrative capability authorising forced ownership transitions.
///
/// This is the *special administrative capability* ADR 0037 requires for a `FORCE`
/// transition — deliberately a separate privilege from ordinary transition
/// authority, so that holding the power to rebalance or fail over does **not**
/// confer the power to bypass quorum. Possessing one is the operator's proof of
/// that privilege; it names the operator principal so the audit trail records *who*
/// forced the transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceTransitionCapability {
    operator: NodeIdentity,
}

impl ForceTransitionCapability {
    /// A capability granted to `operator` — the principal that will be recorded as
    /// having exercised it in the audit evidence.
    pub fn granted_to(operator: NodeIdentity) -> Self {
        Self { operator }
    }

    /// The operator principal this capability was granted to.
    pub fn operator(&self) -> &NodeIdentity {
        &self.operator
    }
}

/// A non-empty, operator-supplied justification for a forced transition.
///
/// ADR 0037 requires an *explicit operator reason*; this newtype makes "explicit"
/// enforceable — it cannot be constructed from blank text, so a forced transition
/// either carries a real justification or is refused for the lack of one. The
/// stored text is trimmed of surrounding whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorReason(String);

/// The operator reason was empty or whitespace-only — not an explicit
/// justification, so it cannot authorise a forced transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyOperatorReason;

impl std::fmt::Display for EmptyOperatorReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "forced transition operator reason must not be empty")
    }
}

impl std::error::Error for EmptyOperatorReason {}

impl OperatorReason {
    /// Build a reason from `text`, rejecting blank (empty or whitespace-only)
    /// input. Surrounding whitespace is trimmed from the stored value.
    pub fn new(text: impl Into<String>) -> Result<Self, EmptyOperatorReason> {
        let text = text.into();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(EmptyOperatorReason);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperatorReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A request to force ownership of one range to `target` for disaster recovery.
///
/// Unlike a [`TransitionRequest`](super::ownership_transition::TransitionRequest)
/// it carries no CAS expectations and no catch-up evidence — force exists precisely
/// for the case where those cannot be satisfied. What it carries instead are the
/// two authorisation inputs ADR 0037 requires: an optional
/// [`ForceTransitionCapability`] and an optional [`OperatorReason`]. They are
/// *optional* on the request so the authorisation gate can observe — and audit —
/// their absence; [`force_transition`] refuses any request missing either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedTransitionRequest {
    collection: CollectionId,
    range_id: RangeId,
    target: NodeIdentity,
    new_replicas: Vec<NodeIdentity>,
    capability: Option<ForceTransitionCapability>,
    reason: Option<OperatorReason>,
}

impl ForcedTransitionRequest {
    /// A forced-transition request for `(collection, range_id)` installing `target`
    /// as the new owner, with **no** capability or reason attached yet. As built it
    /// will be denied; attach authorisation with
    /// [`with_capability`](Self::with_capability) and [`with_reason`](Self::with_reason).
    pub fn new(collection: CollectionId, range_id: RangeId, target: NodeIdentity) -> Self {
        Self {
            collection,
            range_id,
            target,
            new_replicas: Vec::new(),
            capability: None,
            reason: None,
        }
    }

    /// Attach the administrative capability authorising the force.
    pub fn with_capability(mut self, capability: ForceTransitionCapability) -> Self {
        self.capability = Some(capability);
        self
    }

    /// Attach the operator's explicit justification.
    pub fn with_reason(mut self, reason: OperatorReason) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Set the replica set the forced new owner will carry. Defaults to empty — in
    /// a disaster the operator often installs a sole survivor with no replicas.
    pub fn with_replicas(mut self, replicas: impl IntoIterator<Item = NodeIdentity>) -> Self {
        self.new_replicas = replicas.into_iter().collect();
        self
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    pub fn target(&self) -> &NodeIdentity {
        &self.target
    }
}

/// Why a forced transition was refused at the **authorisation** gate, before the
/// catalog was consulted. Distinct from [`ForceFailure`], which is a refusal
/// *after* authorisation passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceDenial {
    /// No [`ForceTransitionCapability`] was presented. Forcing a transition
    /// requires the distinct administrative privilege; without it the request is
    /// refused outright.
    MissingCapability,
    /// No (non-empty) [`OperatorReason`] was attached. A forced transition must
    /// state why quorum is being bypassed.
    MissingReason,
}

impl ForceDenial {
    fn label(self) -> &'static str {
        match self {
            ForceDenial::MissingCapability => "no administrative force capability presented",
            ForceDenial::MissingReason => "no explicit operator reason supplied",
        }
    }
}

impl std::fmt::Display for ForceDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Why an *authorised* forced transition could not be applied to the catalog. The
/// operator held the capability and stated a reason, but the catalog refused the
/// write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForceFailure {
    /// No range with this `(collection, range_id)` exists — there is nothing to
    /// take ownership of.
    UnknownRange,
    /// The catalog rejected the activation write (e.g. a concurrent edit advanced
    /// the version first). The forced entry was not installed.
    Catalog(CatalogError),
}

impl std::fmt::Display for ForceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRange => write!(f, "no such range in the catalog"),
            Self::Catalog(err) => write!(f, "{err}"),
        }
    }
}

/// The disposition of a forced-transition attempt — the verdict recorded in the
/// [`ForcedTransitionAudit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForcedTransitionDisposition {
    /// The force was authorised and applied. Carries the before/after owner,
    /// epoch, and version so the audit record fully describes what moved.
    Allowed {
        previous_owner: NodeIdentity,
        new_owner: NodeIdentity,
        previous_epoch: OwnershipEpoch,
        new_epoch: OwnershipEpoch,
        previous_version: CatalogVersion,
        new_version: CatalogVersion,
    },
    /// The force was refused at the authorisation gate; the catalog was never
    /// touched.
    Denied(ForceDenial),
    /// The force was authorised but the catalog write failed; nothing moved.
    Failed(ForceFailure),
}

/// Durable audit evidence for one forced-transition attempt — emitted for
/// **allowed, denied, and failed** attempts alike.
///
/// This is the *durable audit evidence* ADR 0037 requires. Because a forced
/// transition can bypass quorum, every attempt — including the ones that were
/// refused — must leave a trail; making the audit the return value of
/// [`force_transition`] means a caller cannot perform a force without also
/// receiving its evidence. The record captures *who* (operator), *why* (reason),
/// *what* (collection/range/target), *when* (`attempted_at_ms`), and the
/// [`disposition`](Self::disposition) (the outcome, with the full epoch/version
/// boundary on success).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedTransitionAudit {
    attempted_at_ms: u64,
    operator: Option<NodeIdentity>,
    reason: Option<String>,
    collection: CollectionId,
    range_id: RangeId,
    target: NodeIdentity,
    disposition: ForcedTransitionDisposition,
}

impl ForcedTransitionAudit {
    /// The wall-clock time (ms) the attempt was evaluated, as supplied by the
    /// caller.
    pub fn attempted_at_ms(&self) -> u64 {
        self.attempted_at_ms
    }

    /// The operator principal that exercised the capability, if one was presented.
    /// `None` on an attempt denied for [`MissingCapability`](ForceDenial::MissingCapability).
    pub fn operator(&self) -> Option<&NodeIdentity> {
        self.operator.as_ref()
    }

    /// The operator's stated reason, if one was attached. `None` on an attempt
    /// denied for [`MissingReason`](ForceDenial::MissingReason) (or missing
    /// capability).
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn collection(&self) -> &CollectionId {
        &self.collection
    }

    pub fn range_id(&self) -> RangeId {
        self.range_id
    }

    /// The node the force tried to install as owner.
    pub fn target(&self) -> &NodeIdentity {
        &self.target
    }

    pub fn disposition(&self) -> &ForcedTransitionDisposition {
        &self.disposition
    }

    /// Whether the force was authorised and applied.
    pub fn is_allowed(&self) -> bool {
        matches!(
            self.disposition,
            ForcedTransitionDisposition::Allowed { .. }
        )
    }

    /// Whether a successful force bumped the ownership epoch — true for every
    /// allowed force, since installing a new owner always advances the epoch and so
    /// fences any old owner that later reappears. `false` for denied/failed
    /// attempts, where nothing moved.
    pub fn fenced_old_owner(&self) -> bool {
        matches!(
            self.disposition,
            ForcedTransitionDisposition::Allowed {
                previous_epoch,
                new_epoch,
                ..
            } if new_epoch > previous_epoch
        )
    }
}

impl std::fmt::Display for ForcedTransitionAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operator = self
            .operator
            .as_ref()
            .map(|o| o.to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let reason = self.reason.as_deref().unwrap_or("<none>");
        write!(
            f,
            "forced ownership transition @ {} ms by operator {} for {}/{} -> {} (reason: {}): ",
            self.attempted_at_ms, operator, self.collection, self.range_id, self.target, reason,
        )?;
        match &self.disposition {
            ForcedTransitionDisposition::Allowed {
                previous_owner,
                new_owner,
                previous_epoch,
                new_epoch,
                previous_version,
                new_version,
            } => write!(
                f,
                "ALLOWED: {} (epoch {}, version {}) -> {} (epoch {}, version {})",
                previous_owner, previous_epoch, previous_version, new_owner, new_epoch, new_version,
            ),
            ForcedTransitionDisposition::Denied(reason) => write!(f, "DENIED: {reason}"),
            ForcedTransitionDisposition::Failed(failure) => write!(f, "FAILED: {failure}"),
        }
    }
}

/// Evaluate and, if authorised, apply a forced ownership transition — returning the
/// durable audit evidence in **every** case.
///
/// The authorisation gate is fail-closed and runs before the catalog is consulted:
/// the request must carry a [`ForceTransitionCapability`]
/// ([`MissingCapability`](ForceDenial::MissingCapability) otherwise) and a non-empty
/// [`OperatorReason`] ([`MissingReason`](ForceDenial::MissingReason) otherwise).
/// Only once both hold is the range looked up; a missing range is a
/// [`ForceFailure::UnknownRange`]. On success the new owner is installed via
/// [`RangeOwnership::transfer_to`], bumping the ownership epoch so the old owner is
/// fenced if it reappears.
///
/// Whatever the outcome, the returned [`ForcedTransitionAudit`] records it — the
/// caller persists it as the operation's audit trail. Denied and failed attempts
/// leave the catalog untouched.
pub fn force_transition(
    catalog: &mut ShardOwnershipCatalog,
    request: &ForcedTransitionRequest,
    now_ms: u64,
) -> ForcedTransitionAudit {
    let operator = request.capability.as_ref().map(|c| c.operator().clone());
    let reason = request.reason.as_ref().map(|r| r.as_str().to_string());

    let audit = |disposition| ForcedTransitionAudit {
        attempted_at_ms: now_ms,
        operator: operator.clone(),
        reason: reason.clone(),
        collection: request.collection.clone(),
        range_id: request.range_id,
        target: request.target.clone(),
        disposition,
    };

    // Authorisation gate (fail-closed): the distinct capability first, then the
    // explicit operator reason. Both are required before the catalog is touched.
    if request.capability.is_none() {
        return audit(ForcedTransitionDisposition::Denied(
            ForceDenial::MissingCapability,
        ));
    }
    if request.reason.is_none() {
        return audit(ForcedTransitionDisposition::Denied(
            ForceDenial::MissingReason,
        ));
    }

    // Authorised. Force bypasses CAS / catch-up evidence / replica membership, but
    // the range must exist for there to be ownership to move.
    let Some(current) = catalog.range(&request.collection, request.range_id) else {
        return audit(ForcedTransitionDisposition::Failed(
            ForceFailure::UnknownRange,
        ));
    };

    let previous_owner = current.owner().clone();
    let previous_epoch = current.epoch();
    let previous_version = current.version();
    // transfer_to bumps both epoch (fencing the old owner) and version.
    let next = current.transfer_to(request.target.clone(), request.new_replicas.clone());
    let new_epoch = next.epoch();
    let new_version = next.version();

    match catalog.apply_update(next) {
        Ok(_) => audit(ForcedTransitionDisposition::Allowed {
            previous_owner,
            new_owner: request.target.clone(),
            previous_epoch,
            new_epoch,
            previous_version,
            new_version,
        }),
        Err(err) => audit(ForcedTransitionDisposition::Failed(ForceFailure::Catalog(
            err,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{
        OwnershipEpoch, PlacementMetadata, RangeBounds, RangeWriteReject, ShardKeyMode,
    };

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

    fn capability(operator: &str) -> ForceTransitionCapability {
        ForceTransitionCapability::granted_to(ident(operator))
    }

    fn reason() -> OperatorReason {
        OperatorReason::new("primary AZ lost, promoting surviving copy").unwrap()
    }

    /// A fully-authorised forced request: capability + reason attached.
    fn authorised_request(orders: &CollectionId, target: &str) -> ForcedTransitionRequest {
        ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident(target))
            .with_capability(capability("CN=operator-root"))
            .with_reason(reason())
    }

    // ---------------------------------------------------------------
    // OperatorReason: "explicit" must be enforceable.
    // ---------------------------------------------------------------

    #[test]
    fn operator_reason_rejects_blank_input() {
        assert_eq!(OperatorReason::new(""), Err(EmptyOperatorReason));
        assert_eq!(OperatorReason::new("   "), Err(EmptyOperatorReason));
        assert_eq!(OperatorReason::new("\t\n "), Err(EmptyOperatorReason));
    }

    #[test]
    fn operator_reason_trims_surrounding_whitespace() {
        let r = OperatorReason::new("  recover orders/1  ").unwrap();
        assert_eq!(r.as_str(), "recover orders/1");
    }

    // ---------------------------------------------------------------
    // Authorisation gate: capability and reason are both required.
    // ---------------------------------------------------------------

    #[test]
    fn force_denied_without_capability() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Reason present, but no capability.
        let req = ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident("CN=node-b"))
            .with_reason(reason());

        let audit = force_transition(&mut catalog, &req, 1_000);

        assert!(!audit.is_allowed());
        assert_eq!(
            audit.disposition(),
            &ForcedTransitionDisposition::Denied(ForceDenial::MissingCapability)
        );
        // The catalog is untouched: node-a is still owner at the initial epoch.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.epoch(), OwnershipEpoch::initial());
        // Audit evidence is still emitted for the denied attempt.
        assert!(audit.to_string().contains("DENIED"));
        assert_eq!(audit.attempted_at_ms(), 1_000);
    }

    #[test]
    fn force_denied_without_reason() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Capability present, but no operator reason.
        let req = ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident("CN=node-b"))
            .with_capability(capability("CN=operator-root"));

        let audit = force_transition(&mut catalog, &req, 2_000);

        assert!(!audit.is_allowed());
        assert_eq!(
            audit.disposition(),
            &ForcedTransitionDisposition::Denied(ForceDenial::MissingReason)
        );
        // Operator is recorded (capability was presented) but reason is absent.
        assert_eq!(audit.operator(), Some(&ident("CN=operator-root")));
        assert_eq!(audit.reason(), None);
        // Catalog untouched.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-a"));
    }

    #[test]
    fn missing_capability_is_reported_before_missing_reason() {
        // Fail-closed ordering: with neither present, the capability denial wins.
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = ForcedTransitionRequest::new(orders, RangeId::new(1), ident("CN=node-b"));
        let audit = force_transition(&mut catalog, &req, 0);
        assert_eq!(
            audit.disposition(),
            &ForcedTransitionDisposition::Denied(ForceDenial::MissingCapability)
        );
    }

    // ---------------------------------------------------------------
    // Successful forced transition: epoch bump + audit evidence.
    // ---------------------------------------------------------------

    #[test]
    fn authorised_force_bumps_epoch_and_moves_owner() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // node-b is not even a replica — force can still install it.
        let req = ForcedTransitionRequest::new(orders.clone(), RangeId::new(1), ident("CN=node-z"))
            .with_capability(capability("CN=operator-root"))
            .with_reason(reason());

        let audit = force_transition(&mut catalog, &req, 5_000);

        assert!(audit.is_allowed());
        assert!(audit.fenced_old_owner());
        match audit.disposition() {
            ForcedTransitionDisposition::Allowed {
                previous_owner,
                new_owner,
                previous_epoch,
                new_epoch,
                previous_version,
                new_version,
            } => {
                assert_eq!(previous_owner, &ident("CN=node-a"));
                assert_eq!(new_owner, &ident("CN=node-z"));
                assert_eq!(*previous_epoch, OwnershipEpoch::initial());
                assert_eq!(new_epoch.value(), 2);
                assert_eq!(previous_version.value(), 1);
                assert_eq!(new_version.value(), 2);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }

        // The catalog now makes node-z authoritative at the bumped epoch.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-z"));
        assert_eq!(range.epoch().value(), 2);
    }

    #[test]
    fn audit_evidence_records_operator_reason_and_boundary() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = authorised_request(&orders, "CN=node-b");

        let audit = force_transition(&mut catalog, &req, 7_777);

        assert_eq!(audit.operator(), Some(&ident("CN=operator-root")));
        assert_eq!(
            audit.reason(),
            Some("primary AZ lost, promoting surviving copy")
        );
        assert_eq!(audit.attempted_at_ms(), 7_777);
        assert_eq!(audit.target(), &ident("CN=node-b"));
        let line = audit.to_string();
        assert!(line.contains("ALLOWED"));
        assert!(line.contains("CN=operator-root"));
        assert!(line.contains("primary AZ lost"));
        assert!(line.contains("CN=node-a"));
        assert!(line.contains("CN=node-b"));
    }

    // ---------------------------------------------------------------
    // Old owner is fenced once it reappears after a force.
    // ---------------------------------------------------------------

    #[test]
    fn reappearing_old_owner_is_fenced_after_force() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Before the force node-a (the owner) is admitted at the initial epoch.
        assert!(catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial()
            )
            .is_ok());

        // Force ownership to node-b, demoting node-a to a replica so role alone
        // would not fence it — only the epoch bump does.
        let req = authorised_request(&orders, "CN=node-b").with_replicas([ident("CN=node-a")]);
        let audit = force_transition(&mut catalog, &req, 1_000);
        assert!(audit.is_allowed());

        // node-a reappears (partition healed) still believing epoch 1. Its write is
        // fenced: as a replica it is no longer the owner...
        let err = catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        assert!(matches!(err, RangeWriteReject::NotOwner { .. }));

        // ...and even an old owner that still believed itself owner would carry the
        // stale epoch; node-b at the bumped epoch is the one now admitted.
        let current_epoch = catalog.range(&orders, RangeId::new(1)).unwrap().epoch();
        assert!(catalog
            .admit_public_write(&ident("CN=node-b"), &orders, b"k", current_epoch)
            .is_ok());
    }

    #[test]
    fn force_against_unknown_range_fails_and_is_audited() {
        let mut catalog = ShardOwnershipCatalog::new();
        let orders = collection("orders");
        // Authorised, but the range does not exist.
        let req = authorised_request(&orders, "CN=node-b");

        let audit = force_transition(&mut catalog, &req, 3_000);

        assert!(!audit.is_allowed());
        assert_eq!(
            audit.disposition(),
            &ForcedTransitionDisposition::Failed(ForceFailure::UnknownRange)
        );
        // Failed attempts still carry full operator/reason evidence.
        assert_eq!(audit.operator(), Some(&ident("CN=operator-root")));
        assert!(audit.reason().is_some());
        assert!(audit.to_string().contains("FAILED"));
    }

    #[test]
    fn ordinary_safety_checks_are_untouched_by_the_force_path() {
        // The force path neither weakens nor invokes the ordinary transition gate:
        // an ordinary transition with a stale CAS still loses. This guards the
        // acceptance criterion that non-force operations keep their safety checks.
        use crate::cluster::ownership_transition::{
            run_transition, CommitWatermark, TransitionError, TransitionKind, TransitionRejection,
            TransitionRequest,
        };
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);

        // First, a legitimate force moves authority to node-b (epoch -> 2).
        let forced = force_transition(&mut catalog, &authorised_request(&orders, "CN=node-b"), 10);
        assert!(forced.is_allowed());

        // An ordinary transition planner still holding the pre-force CAS (node-a at
        // epoch 1) is rejected by the ordinary gate — force did not disable it.
        let stale = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            CatalogVersion::initial(),
            ident("CN=node-b"),
            CommitWatermark::new(1, 10),
        )
        .with_evidence(crate::cluster::ownership_transition::CatchUpEvidence::new(
            ident("CN=node-b"),
            1,
            10,
        ));
        let err = run_transition(&mut catalog, &stale).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::Rejected(TransitionRejection::OwnerMismatch { .. })
        ));
    }
}
