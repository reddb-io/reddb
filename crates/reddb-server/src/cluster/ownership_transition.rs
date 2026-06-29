//! Ownership transition state machine for promote and fenced handoff
//! (issue #995, PRD #987, ADR 0037).
//!
//! [`ShardOwnershipCatalog`] (issue #989) is the source of truth for *where* a
//! range lives; this module is the only sanctioned way to *change* that. Per
//! ADR 0037, "ownership changes are transitions, not arbitrary row edits" and
//! "rebalancing, failover, and administrative recovery become catalog
//! transitions" — so failover-promote and move-range handoff both funnel through
//! the one fenced, versioned, audited machine here.
//!
//! ## Why a state machine rather than a `transfer_to` call
//!
//! [`RangeOwnership::transfer_to`] already bumps the epoch and version, but it is
//! an *unconditional* edit: hand it any owner and it produces a new entry. That
//! is unsafe as a control-plane operation, because a transition that races a
//! concurrent one, names the wrong current owner, or activates a candidate that
//! has not caught up to the range commit watermark must **fail closed**, not
//! silently move authority. This machine adds the compare-and-swap and the safety
//! gate around that edit:
//!
//! 1. **Prepare** ([`prepare`]) — a pure check against the catalog. The request
//!    must name the *expected current owner*, *expected epoch*, and *expected
//!    catalog version* (a three-part CAS, so a stale planner loses), a *valid
//!    target candidate* (a current replica of the range — only a replica that
//!    covers the range commit watermark may be promoted, per the glossary), and
//!    *safety evidence* that the candidate's applied log covers the range commit
//!    watermark. Any failure yields a [`TransitionRejection`] and the catalog is
//!    never touched.
//! 2. **Activate** ([`PreparedTransition::activate`]) — only reachable *after*
//!    prepare succeeds, this applies the fenced transition to the catalog. The
//!    epoch bump in the new entry is what fences the old owner: from this point
//!    its writes carry a stale epoch and [`admit_public_write`] rejects them.
//!
//! Splitting prepare from activate is the literal encoding of the acceptance
//! criterion "activate new owners only after safety checks": you cannot obtain a
//! [`PreparedTransition`] without passing every check, and activation is a
//! distinct, explicit second step.
//!
//! ## Promote vs. handoff
//!
//! Both kinds run the identical safety gate — the difference is *intent and
//! audit*, recorded in [`TransitionKind`]:
//!
//! * [`Promote`](TransitionKind::Promote) — failover. The recorded owner is gone
//!   (or being deposed); a caught-up replica takes authority. The old owner is
//!   fenced by the epoch bump.
//! * [`Handoff`](TransitionKind::Handoff) — move-range cutover. The current owner
//!   keeps serving until the target has copied and caught up; only then does this
//!   transition move the epoch, fencing the old owner at the cutover instant.
//!
//! Forced (disaster-recovery) transitions, which may proceed without ordinary
//! safety checks, are out of scope here: ADR 0037 reserves them for a separate
//! `FORCE` capability path, implemented in
//! [`ownership_force`](super::ownership_force).
//!
//! Everything is a pure data model over the catalog, with no I/O, so the CAS,
//! fencing, and audit story is exercised deterministically.

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogError, CatalogVersion, CollectionId, OwnershipEpoch, RangeId, RangeOwnership, RangeRole,
    ShardOwnershipCatalog,
};

/// The highest `(term, lsn)` known durable for a range under its commit policy
/// — the *range commit watermark*. Per the glossary, "failover and interrupted
/// move-range recovery may promote only a candidate whose log covers this
/// watermark", so it is the bar a transition's safety evidence must clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitWatermark {
    /// The owning term at the watermark. A candidate on an older term has not
    /// observed the latest authority and cannot be promoted.
    pub term: u64,
    /// The highest durable WAL LSN for the range.
    pub lsn: u64,
}

impl CommitWatermark {
    pub fn new(term: u64, lsn: u64) -> Self {
        Self { term, lsn }
    }
}

/// Evidence that a candidate has caught up enough to take ownership safely: the
/// `(term, lsn)` its log has durably applied **for the range**. The supervisor
/// collects this from the candidate's per-range stream progress (issue #992)
/// before requesting a transition; the machine admits the candidate only if this
/// covers the [`CommitWatermark`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpEvidence {
    /// The node this evidence describes. Must match the transition target, so a
    /// planner cannot present one node's progress to promote another.
    pub candidate: NodeIdentity,
    /// The highest term the candidate has applied for the range.
    pub applied_term: u64,
    /// The highest WAL LSN the candidate has applied for the range.
    pub applied_lsn: u64,
}

impl CatchUpEvidence {
    pub fn new(candidate: NodeIdentity, applied_term: u64, applied_lsn: u64) -> Self {
        Self {
            candidate,
            applied_term,
            applied_lsn,
        }
    }

    /// Does this evidence cover the watermark? The candidate must be on at least
    /// the watermark term **and**, on that term, have applied at least its LSN. A
    /// candidate behind on either axis is fenced out of promotion.
    pub fn covers(&self, watermark: CommitWatermark) -> bool {
        self.applied_term > watermark.term
            || (self.applied_term == watermark.term && self.applied_lsn >= watermark.lsn)
    }
}

/// Validation receipts required before a compressed archive copy may be used as
/// a restored recovery source. This is deliberately separate from
/// [`CatchUpEvidence`]: hot mirror promotion needs live catch-up evidence, while
/// archive recovery must first prove restore integrity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveRecoveryEvidence {
    pub restore_validated: bool,
    pub checksum_validated: bool,
    pub watermark_validated: bool,
}

impl ArchiveRecoveryEvidence {
    pub fn new(
        restore_validated: bool,
        checksum_validated: bool,
        watermark_validated: bool,
    ) -> Self {
        Self {
            restore_validated,
            checksum_validated,
            watermark_validated,
        }
    }
}

/// Why a compressed archive copy is not eligible to be used as a recovery
/// source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveRecoveryRejection {
    NotArchiveReplica {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
        role: RangeRole,
    },
    MissingRestoreValidation {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
    },
    MissingChecksumValidation {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
    },
    MissingWatermarkValidation {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
    },
}

/// Whether a transition is a failover promote or a move-range handoff. Both run
/// the same safety gate; the kind is recorded for the audit trail and intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    /// Failover promote: a caught-up replica takes authority from a gone/deposed
    /// owner.
    Promote,
    /// Move-range fenced handoff: authority cuts over from a live owner to a
    /// caught-up target.
    Handoff,
}

impl TransitionKind {
    fn label(self) -> &'static str {
        match self {
            TransitionKind::Promote => "promote",
            TransitionKind::Handoff => "handoff",
        }
    }
}

/// A request to move ownership of one range. Carries the three-part CAS
/// (expected owner / epoch / catalog version), the target candidate, the range
/// commit watermark the candidate must cover, and the candidate's catch-up
/// evidence. Built with [`TransitionRequest::new`]; the replica set the new owner
/// will carry defaults to empty and is set with
/// [`with_replicas`](TransitionRequest::with_replicas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRequest {
    kind: TransitionKind,
    collection: CollectionId,
    range_id: RangeId,
    expected_owner: NodeIdentity,
    expected_epoch: OwnershipEpoch,
    expected_version: CatalogVersion,
    target: NodeIdentity,
    watermark: CommitWatermark,
    evidence: Option<CatchUpEvidence>,
    new_replicas: Vec<NodeIdentity>,
}

impl TransitionRequest {
    /// A transition request with no safety evidence yet and an empty post-cutover
    /// replica set. Evidence must be attached with
    /// [`with_evidence`](Self::with_evidence) before the transition can be
    /// admitted — a request without it fails closed
    /// ([`MissingSafetyEvidence`](TransitionRejection::MissingSafetyEvidence)).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: TransitionKind,
        collection: CollectionId,
        range_id: RangeId,
        expected_owner: NodeIdentity,
        expected_epoch: OwnershipEpoch,
        expected_version: CatalogVersion,
        target: NodeIdentity,
        watermark: CommitWatermark,
    ) -> Self {
        Self {
            kind,
            collection,
            range_id,
            expected_owner,
            expected_epoch,
            expected_version,
            target,
            watermark,
            evidence: None,
            new_replicas: Vec::new(),
        }
    }

    /// Attach the candidate's catch-up evidence for the safety gate.
    pub fn with_evidence(mut self, evidence: CatchUpEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Set the replica set the new owner will carry after cutover. Defaults to
    /// empty. A handoff that demotes the old owner to a replica passes it here.
    pub fn with_replicas(mut self, replicas: impl IntoIterator<Item = NodeIdentity>) -> Self {
        self.new_replicas = replicas.into_iter().collect();
        self
    }

    pub fn kind(&self) -> TransitionKind {
        self.kind
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

/// Why a target candidate is not eligible to take ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCandidateReason {
    /// The target is not a current replica of the range. Only a replica that has
    /// been receiving the range's stream can cover the commit watermark, so an
    /// arbitrary node is never a valid promotion target.
    NotAReplica,
    /// The target is a compressed archive replica. Archive data may be a
    /// recovery source only after restore, checksum validation, and watermark
    /// validation prove the recovered range is safe.
    ArchiveReplicaRequiresRestoreValidation,
    /// The target is already the current owner — a transition to the incumbent is
    /// a no-op and almost always a planner bug, so it is rejected.
    AlreadyOwner,
}

impl InvalidCandidateReason {
    fn label(self) -> &'static str {
        match self {
            InvalidCandidateReason::NotAReplica => "candidate is not a replica of the range",
            InvalidCandidateReason::ArchiveReplicaRequiresRestoreValidation => {
                "archive replica requires restore, checksum, and watermark validation"
            }
            InvalidCandidateReason::AlreadyOwner => "candidate is already the current owner",
        }
    }
}

/// Why an ownership transition was refused. Every variant leaves the catalog
/// untouched — transitions fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionRejection {
    /// No range with this `(collection, range_id)` exists in the catalog.
    UnknownRange {
        collection: CollectionId,
        range_id: RangeId,
    },
    /// The request's expected current owner does not match the catalog. The
    /// planner is working from a stale view of who owns the range.
    OwnerMismatch {
        collection: CollectionId,
        range_id: RangeId,
        expected: NodeIdentity,
        current: NodeIdentity,
    },
    /// The request's expected ownership epoch does not match the catalog —
    /// authority has already moved since the planner read it.
    StaleEpoch {
        collection: CollectionId,
        range_id: RangeId,
        expected: OwnershipEpoch,
        current: OwnershipEpoch,
    },
    /// The request's expected catalog version does not match the catalog — the
    /// entry has been edited since the planner read it (CAS failure).
    StaleCatalogVersion {
        collection: CollectionId,
        range_id: RangeId,
        expected: CatalogVersion,
        current: CatalogVersion,
    },
    /// The target candidate is not eligible to take ownership.
    InvalidCandidate {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
        reason: InvalidCandidateReason,
    },
    /// No safety evidence was supplied for the candidate, so the safety gate
    /// cannot be evaluated — fail closed.
    MissingSafetyEvidence {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
    },
    /// Safety evidence was supplied but describes a different node than the
    /// target — it cannot vouch for the candidate being promoted.
    EvidenceForWrongCandidate {
        collection: CollectionId,
        range_id: RangeId,
        target: NodeIdentity,
        evidence_for: NodeIdentity,
    },
    /// The candidate's applied log does not cover the range commit watermark —
    /// promoting it could lose committed writes, so the transition is refused.
    SafetyCheckFailed {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
        watermark: CommitWatermark,
        applied_term: u64,
        applied_lsn: u64,
    },
}

impl std::fmt::Display for TransitionRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRange {
                collection,
                range_id,
            } => write!(f, "no range {collection}/{range_id} in the catalog"),
            Self::OwnerMismatch {
                collection,
                range_id,
                expected,
                current,
            } => write!(
                f,
                "ownership transition for {collection}/{range_id} expected current owner {expected}, but catalog owner is {current}"
            ),
            Self::StaleEpoch {
                collection,
                range_id,
                expected,
                current,
            } => write!(
                f,
                "ownership transition for {collection}/{range_id} expected epoch {expected}, but catalog epoch is {current}"
            ),
            Self::StaleCatalogVersion {
                collection,
                range_id,
                expected,
                current,
            } => write!(
                f,
                "ownership transition for {collection}/{range_id} expected catalog version {expected}, but catalog version is {current}"
            ),
            Self::InvalidCandidate {
                collection,
                range_id,
                candidate,
                reason,
            } => write!(
                f,
                "invalid ownership transition candidate {candidate} for {collection}/{range_id}: {}",
                reason.label()
            ),
            Self::MissingSafetyEvidence {
                collection,
                range_id,
                candidate,
            } => write!(
                f,
                "ownership transition for {collection}/{range_id} carries no safety evidence for candidate {candidate}"
            ),
            Self::EvidenceForWrongCandidate {
                collection,
                range_id,
                target,
                evidence_for,
            } => write!(
                f,
                "ownership transition for {collection}/{range_id} targets {target} but its safety evidence describes {evidence_for}"
            ),
            Self::SafetyCheckFailed {
                collection,
                range_id,
                candidate,
                watermark,
                applied_term,
                applied_lsn,
            } => write!(
                f,
                "candidate {candidate} for {collection}/{range_id} is behind the commit watermark (term {}, lsn {}): applied term {applied_term}, lsn {applied_lsn}",
                watermark.term, watermark.lsn
            ),
        }
    }
}

impl std::error::Error for TransitionRejection {}

/// A validated, not-yet-applied ownership transition. Holding one is proof that
/// every CAS and safety check passed; the only thing left is to
/// [`activate`](Self::activate) it against the catalog. It carries the full
/// before/after picture so it doubles as the audit record source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTransition {
    kind: TransitionKind,
    collection: CollectionId,
    range_id: RangeId,
    previous_owner: NodeIdentity,
    new_owner: NodeIdentity,
    previous_epoch: OwnershipEpoch,
    previous_version: CatalogVersion,
    watermark: CommitWatermark,
    next: RangeOwnership,
}

impl PreparedTransition {
    /// The new ownership entry that activation will install — epoch and version
    /// already advanced past the current entry.
    pub fn next_entry(&self) -> &RangeOwnership {
        &self.next
    }

    /// The ownership epoch the new owner becomes authoritative under. Any write
    /// the old owner attempts under the previous epoch is fenced once this is
    /// installed.
    pub fn new_epoch(&self) -> OwnershipEpoch {
        self.next.epoch()
    }

    /// Apply the transition to the catalog, making the new owner authoritative
    /// and fencing the old owner via the epoch bump. Returns the audit-ready
    /// [`TransitionOutcome`]. Errors only on a catalog-level inconsistency
    /// (e.g. the entry changed between prepare and activate so the version no
    /// longer strictly advances) — the safety decision itself was already made.
    pub fn activate(
        self,
        catalog: &mut ShardOwnershipCatalog,
    ) -> Result<TransitionOutcome, CatalogError> {
        let new_epoch = self.next.epoch();
        let new_version = self.next.version();
        catalog.apply_update(self.next)?;
        Ok(TransitionOutcome {
            kind: self.kind,
            collection: self.collection,
            range_id: self.range_id,
            previous_owner: self.previous_owner,
            new_owner: self.new_owner,
            previous_epoch: self.previous_epoch,
            new_epoch,
            previous_version: self.previous_version,
            new_version,
            watermark: self.watermark,
        })
    }
}

/// The audit-ready record of an activated ownership transition. Every field a
/// reviewer needs to reconstruct *what moved, from whom to whom, and across which
/// epoch/version boundary* — the fenced before/after the ADR's audit requirement
/// asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    pub kind: TransitionKind,
    pub collection: CollectionId,
    pub range_id: RangeId,
    pub previous_owner: NodeIdentity,
    pub new_owner: NodeIdentity,
    pub previous_epoch: OwnershipEpoch,
    pub new_epoch: OwnershipEpoch,
    pub previous_version: CatalogVersion,
    pub new_version: CatalogVersion,
    pub watermark: CommitWatermark,
}

impl TransitionOutcome {
    /// Whether the epoch advanced — true for every accepted transition, since
    /// moving write authority always fences the old owner. A handy invariant for
    /// audit assertions.
    pub fn fenced_old_owner(&self) -> bool {
        self.new_epoch > self.previous_epoch
    }
}

impl std::fmt::Display for TransitionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}/{}: {} (epoch {}, version {}) -> {} (epoch {}, version {}) over watermark term {} lsn {}",
            self.kind.label(),
            self.collection,
            self.range_id,
            self.previous_owner,
            self.previous_epoch,
            self.previous_version,
            self.new_owner,
            self.new_epoch,
            self.new_version,
            self.watermark.term,
            self.watermark.lsn,
        )
    }
}

/// Validate `request` against `catalog` without mutating anything. On success
/// returns a [`PreparedTransition`] ready to [`activate`]; on any failed CAS or
/// safety check returns a [`TransitionRejection`] and leaves the catalog
/// untouched.
///
/// Checks run in fail-closed order: the range must exist; the expected owner,
/// epoch, and catalog version must all match (CAS); the target must be a current
/// replica that is not already the owner; and the candidate's safety evidence
/// must be present, describe the target, and cover the commit watermark.
///
/// [`activate`]: PreparedTransition::activate
pub fn prepare(
    catalog: &ShardOwnershipCatalog,
    request: &TransitionRequest,
) -> Result<PreparedTransition, TransitionRejection> {
    let current = catalog.range(&request.collection, request.range_id).ok_or(
        TransitionRejection::UnknownRange {
            collection: request.collection.clone(),
            range_id: request.range_id,
        },
    )?;

    // Three-part compare-and-swap: a planner working from a stale catalog view
    // loses on whichever axis drifted first.
    if *current.owner() != request.expected_owner {
        return Err(TransitionRejection::OwnerMismatch {
            collection: request.collection.clone(),
            range_id: request.range_id,
            expected: request.expected_owner.clone(),
            current: current.owner().clone(),
        });
    }
    if current.epoch() != request.expected_epoch {
        return Err(TransitionRejection::StaleEpoch {
            collection: request.collection.clone(),
            range_id: request.range_id,
            expected: request.expected_epoch,
            current: current.epoch(),
        });
    }
    if current.version() != request.expected_version {
        return Err(TransitionRejection::StaleCatalogVersion {
            collection: request.collection.clone(),
            range_id: request.range_id,
            expected: request.expected_version,
            current: current.version(),
        });
    }

    // Candidate eligibility: a valid target is a hot replica of the range and
    // not the incumbent owner. Compressed archive replicas are recovery sources,
    // not direct promotion candidates.
    match current.role_of(&request.target) {
        RangeRole::Owner => {
            return Err(TransitionRejection::InvalidCandidate {
                collection: request.collection.clone(),
                range_id: request.range_id,
                candidate: request.target.clone(),
                reason: InvalidCandidateReason::AlreadyOwner,
            });
        }
        RangeRole::Replica => {}
        RangeRole::ArchiveReplica => {
            return Err(TransitionRejection::InvalidCandidate {
                collection: request.collection.clone(),
                range_id: request.range_id,
                candidate: request.target.clone(),
                reason: InvalidCandidateReason::ArchiveReplicaRequiresRestoreValidation,
            });
        }
        RangeRole::NoCopy => {
            return Err(TransitionRejection::InvalidCandidate {
                collection: request.collection.clone(),
                range_id: request.range_id,
                candidate: request.target.clone(),
                reason: InvalidCandidateReason::NotAReplica,
            });
        }
    }

    // Safety gate: evidence must exist, vouch for the target, and cover the
    // range commit watermark.
    let evidence =
        request
            .evidence
            .as_ref()
            .ok_or_else(|| TransitionRejection::MissingSafetyEvidence {
                collection: request.collection.clone(),
                range_id: request.range_id,
                candidate: request.target.clone(),
            })?;
    if evidence.candidate != request.target {
        return Err(TransitionRejection::EvidenceForWrongCandidate {
            collection: request.collection.clone(),
            range_id: request.range_id,
            target: request.target.clone(),
            evidence_for: evidence.candidate.clone(),
        });
    }
    if !evidence.covers(request.watermark) {
        return Err(TransitionRejection::SafetyCheckFailed {
            collection: request.collection.clone(),
            range_id: request.range_id,
            candidate: request.target.clone(),
            watermark: request.watermark,
            applied_term: evidence.applied_term,
            applied_lsn: evidence.applied_lsn,
        });
    }

    // All checks passed — build the fenced transition (epoch + version bumped).
    let next = current.transfer_to(request.target.clone(), request.new_replicas.clone());
    Ok(PreparedTransition {
        kind: request.kind,
        collection: request.collection.clone(),
        range_id: request.range_id,
        previous_owner: current.owner().clone(),
        new_owner: request.target.clone(),
        previous_epoch: current.epoch(),
        previous_version: current.version(),
        watermark: request.watermark,
        next,
    })
}

/// Prepare and activate a transition in one step — the common path when the
/// caller does not need to inspect the [`PreparedTransition`] between the safety
/// check and the catalog write. A [`TransitionRejection`] from prepare is mapped
/// into the returned error.
pub fn run_transition(
    catalog: &mut ShardOwnershipCatalog,
    request: &TransitionRequest,
) -> Result<TransitionOutcome, TransitionError> {
    let prepared = prepare(catalog, request)?;
    prepared.activate(catalog).map_err(TransitionError::Catalog)
}

/// Validate that a compressed archive copy may be used as a recovery source.
/// This never prepares a direct ownership transition; a recovered range must be
/// restored and then enter the ordinary hot-replica path before promotion.
pub fn validate_archive_recovery_source(
    catalog: &ShardOwnershipCatalog,
    collection: &CollectionId,
    range_id: RangeId,
    candidate: &NodeIdentity,
    evidence: ArchiveRecoveryEvidence,
) -> Result<(), ArchiveRecoveryRejection> {
    let role = catalog
        .role_at(candidate, collection, range_id)
        .unwrap_or(RangeRole::NoCopy);
    if role != RangeRole::ArchiveReplica {
        return Err(ArchiveRecoveryRejection::NotArchiveReplica {
            collection: collection.clone(),
            range_id,
            candidate: candidate.clone(),
            role,
        });
    }
    if !evidence.restore_validated {
        return Err(ArchiveRecoveryRejection::MissingRestoreValidation {
            collection: collection.clone(),
            range_id,
            candidate: candidate.clone(),
        });
    }
    if !evidence.checksum_validated {
        return Err(ArchiveRecoveryRejection::MissingChecksumValidation {
            collection: collection.clone(),
            range_id,
            candidate: candidate.clone(),
        });
    }
    if !evidence.watermark_validated {
        return Err(ArchiveRecoveryRejection::MissingWatermarkValidation {
            collection: collection.clone(),
            range_id,
            candidate: candidate.clone(),
        });
    }
    Ok(())
}

/// The error of an end-to-end [`run_transition`]: either the safety gate refused
/// the transition, or the catalog rejected the activation write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// A CAS or safety check failed during prepare.
    Rejected(TransitionRejection),
    /// The catalog refused the activation write.
    Catalog(CatalogError),
}

impl From<TransitionRejection> for TransitionError {
    fn from(value: TransitionRejection) -> Self {
        TransitionError::Rejected(value)
    }
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(err) => write!(f, "{err}"),
            Self::Catalog(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for TransitionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{
        OwnershipEpoch, PlacementMetadata, RangeBounds, RangeRole, RangeWriteReject, ShardKeyMode,
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

    fn catalog_with_archive(
        owner: &str,
        replicas: &[&str],
        archive_replicas: &[&str],
    ) -> (ShardOwnershipCatalog, CollectionId) {
        let (mut catalog, orders) = catalog_with(owner, replicas);
        let archived = catalog
            .range(&orders, RangeId::new(1))
            .unwrap()
            .update_archive_replicas(archive_replicas.iter().map(|r| ident(r)));
        catalog.apply_update(archived).unwrap();
        (catalog, orders)
    }

    /// A request that, by default, names the catalog's current owner/epoch/version
    /// and a watermark/evidence the candidate covers.
    fn request(
        kind: TransitionKind,
        orders: &CollectionId,
        expected_owner: &str,
        target: &str,
    ) -> TransitionRequest {
        TransitionRequest::new(
            kind,
            orders.clone(),
            RangeId::new(1),
            ident(expected_owner),
            OwnershipEpoch::initial(),
            CatalogVersion::initial(),
            ident(target),
            CommitWatermark::new(1, 10),
        )
        .with_evidence(CatchUpEvidence::new(ident(target), 1, 10))
    }

    #[test]
    fn successful_promote_moves_authority_and_bumps_epoch() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b");

        let outcome = run_transition(&mut catalog, &req).expect("promote should succeed");

        assert_eq!(outcome.kind, TransitionKind::Promote);
        assert_eq!(outcome.previous_owner, ident("CN=node-a"));
        assert_eq!(outcome.new_owner, ident("CN=node-b"));
        assert_eq!(outcome.previous_epoch, OwnershipEpoch::initial());
        assert_eq!(outcome.new_epoch.value(), 2);
        assert_eq!(outcome.new_version.value(), 2);
        assert!(outcome.fenced_old_owner());

        // The catalog now makes node-b authoritative for the range.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-b"));
        assert_eq!(range.epoch().value(), 2);
        // Audit string mentions both owners and the kind.
        let audit = outcome.to_string();
        assert!(audit.contains("promote"));
        assert!(audit.contains("CN=node-a"));
        assert!(audit.contains("CN=node-b"));
    }

    #[test]
    fn successful_handoff_demotes_old_owner_to_replica() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Move-range cutover: hand off to node-b, keep node-a as a replica.
        let req = request(TransitionKind::Handoff, &orders, "CN=node-a", "CN=node-b")
            .with_replicas([ident("CN=node-a")]);

        let outcome = run_transition(&mut catalog, &req).expect("handoff should succeed");
        assert_eq!(outcome.kind, TransitionKind::Handoff);

        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-b"));
        assert_eq!(range.role_of(&ident("CN=node-a")), RangeRole::Replica);
        assert_eq!(range.role_of(&ident("CN=node-b")), RangeRole::Owner);
    }

    #[test]
    fn old_owner_is_fenced_from_durable_writes_after_transition() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Before the transition node-a is admitted at the initial epoch.
        assert!(catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial()
            )
            .is_ok());

        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b")
            .with_replicas([ident("CN=node-a")]);
        run_transition(&mut catalog, &req).unwrap();

        // node-a, now a replica at the *old* epoch, is fenced: it is no longer the
        // owner, so a public durable write is rejected.
        let err = catalog
            .admit_public_write(
                &ident("CN=node-a"),
                &orders,
                b"k",
                OwnershipEpoch::initial(),
            )
            .unwrap_err();
        assert!(matches!(err, RangeWriteReject::NotOwner { .. }));

        // Even if node-a still believed it was owner, its old epoch is stale.
        // node-b at the new epoch is the one admitted.
        assert!(catalog
            .admit_public_write(
                &ident("CN=node-b"),
                &orders,
                b"k",
                catalog.range(&orders, RangeId::new(1)).unwrap().epoch()
            )
            .is_ok());
    }

    #[test]
    fn prepare_does_not_mutate_catalog() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b");
        let _prepared = prepare(&catalog, &req).expect("prepare ok");
        // Catalog still has node-a as owner at the initial epoch — activation is a
        // separate step.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.epoch(), OwnershipEpoch::initial());
    }

    /// The version/epoch a single unapplied `transfer_to` would advance the
    /// current range to — used to obtain non-initial values without the private
    /// `next()` constructor.
    fn bumped(catalog: &ShardOwnershipCatalog, orders: &CollectionId) -> RangeOwnership {
        catalog
            .range(orders, RangeId::new(1))
            .unwrap()
            .transfer_to(ident("CN=tmp"), [])
    }

    #[test]
    fn stale_catalog_version_is_rejected() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Advance the entry to version 2 without moving the owner or epoch
        // (a replica-set change), so only the planner's version is stale.
        let v2_entry = catalog
            .range(&orders, RangeId::new(1))
            .unwrap()
            .update_replicas([ident("CN=node-b")]);
        catalog.apply_update(v2_entry).unwrap();

        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b");
        // The default request still carries the initial (stale) catalog version.
        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::StaleCatalogVersion {
                expected, current, ..
            } => {
                assert_eq!(expected, CatalogVersion::initial());
                assert_eq!(current.value(), 2);
            }
            other => panic!("expected StaleCatalogVersion, got {other:?}"),
        }
    }

    #[test]
    fn stale_expected_owner_is_rejected() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Planner believes node-x owns the range.
        let req = request(TransitionKind::Promote, &orders, "CN=node-x", "CN=node-b");
        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::OwnerMismatch {
                expected, current, ..
            } => {
                assert_eq!(expected, ident("CN=node-x"));
                assert_eq!(current, ident("CN=node-a"));
            }
            other => panic!("expected OwnerMismatch, got {other:?}"),
        }
    }

    #[test]
    fn stale_expected_epoch_is_rejected() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // A non-initial epoch value (2), obtained without the private `next()`.
        let wrong_epoch = bumped(&catalog, &orders).epoch();
        assert_eq!(wrong_epoch.value(), 2);
        let req = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            wrong_epoch,
            CatalogVersion::initial(),
            ident("CN=node-b"),
            CommitWatermark::new(1, 10),
        )
        .with_evidence(CatchUpEvidence::new(ident("CN=node-b"), 1, 10));
        let err = prepare(&catalog, &req).unwrap_err();
        assert!(matches!(err, TransitionRejection::StaleEpoch { .. }));
    }

    #[test]
    fn invalid_candidate_not_a_replica_is_rejected() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // node-z holds no copy of the range.
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-z");
        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::InvalidCandidate { reason, .. } => {
                assert_eq!(reason, InvalidCandidateReason::NotAReplica);
            }
            other => panic!("expected InvalidCandidate(NotAReplica), got {other:?}"),
        }
    }

    #[test]
    fn archive_replica_is_not_a_direct_promotion_candidate() {
        let (catalog, orders) =
            catalog_with_archive("CN=node-a", &["CN=node-b"], &["CN=archive-a"]);
        let version = catalog.range(&orders, RangeId::new(1)).unwrap().version();
        let req = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            version,
            ident("CN=archive-a"),
            CommitWatermark::new(1, 10),
        )
        .with_evidence(CatchUpEvidence::new(ident("CN=archive-a"), 1, 10));

        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::InvalidCandidate { reason, .. } => {
                assert_eq!(
                    reason,
                    InvalidCandidateReason::ArchiveReplicaRequiresRestoreValidation
                );
            }
            other => panic!("expected archive replica rejection, got {other:?}"),
        }
    }

    #[test]
    fn archive_recovery_rejects_missing_restore_validation() {
        let (catalog, orders) = catalog_with_archive("CN=node-a", &[], &["CN=archive-a"]);
        let err = validate_archive_recovery_source(
            &catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=archive-a"),
            ArchiveRecoveryEvidence::new(false, true, true),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ArchiveRecoveryRejection::MissingRestoreValidation { .. }
        ));
    }

    #[test]
    fn archive_recovery_rejects_missing_checksum_validation() {
        let (catalog, orders) = catalog_with_archive("CN=node-a", &[], &["CN=archive-a"]);
        let err = validate_archive_recovery_source(
            &catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=archive-a"),
            ArchiveRecoveryEvidence::new(true, false, true),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ArchiveRecoveryRejection::MissingChecksumValidation { .. }
        ));
    }

    #[test]
    fn archive_recovery_rejects_missing_watermark_validation() {
        let (catalog, orders) = catalog_with_archive("CN=node-a", &[], &["CN=archive-a"]);
        let err = validate_archive_recovery_source(
            &catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=archive-a"),
            ArchiveRecoveryEvidence::new(true, true, false),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ArchiveRecoveryRejection::MissingWatermarkValidation { .. }
        ));
    }

    #[test]
    fn archive_recovery_and_hot_mirror_promotion_use_different_paths() {
        let (mut catalog, orders) =
            catalog_with_archive("CN=node-a", &["CN=node-b"], &["CN=archive-a"]);
        let version = catalog.range(&orders, RangeId::new(1)).unwrap().version();

        let hot_mirror = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            version,
            ident("CN=node-b"),
            CommitWatermark::new(1, 10),
        )
        .with_evidence(CatchUpEvidence::new(ident("CN=node-b"), 1, 10));
        run_transition(&mut catalog, &hot_mirror).expect("hot mirror promotion succeeds");

        let (catalog, orders) =
            catalog_with_archive("CN=node-a", &["CN=node-b"], &["CN=archive-a"]);
        validate_archive_recovery_source(
            &catalog,
            &orders,
            RangeId::new(1),
            &ident("CN=archive-a"),
            ArchiveRecoveryEvidence::new(true, true, true),
        )
        .expect("archive recovery accepts only after restore integrity validation");
    }

    #[test]
    fn invalid_candidate_already_owner_is_rejected() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Targeting the incumbent owner is a no-op transition.
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-a")
            .with_evidence(CatchUpEvidence::new(ident("CN=node-a"), 1, 10));
        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::InvalidCandidate { reason, .. } => {
                assert_eq!(reason, InvalidCandidateReason::AlreadyOwner);
            }
            other => panic!("expected InvalidCandidate(AlreadyOwner), got {other:?}"),
        }
    }

    #[test]
    fn missing_safety_evidence_fails_closed() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            CatalogVersion::initial(),
            ident("CN=node-b"),
            CommitWatermark::new(1, 10),
        ); // no evidence attached
        let err = prepare(&catalog, &req).unwrap_err();
        assert!(matches!(
            err,
            TransitionRejection::MissingSafetyEvidence { .. }
        ));
    }

    #[test]
    fn evidence_for_a_different_candidate_is_rejected() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Target node-b but present node-c's progress as the evidence.
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b")
            .with_evidence(CatchUpEvidence::new(ident("CN=node-c"), 9, 99));
        let err = prepare(&catalog, &req).unwrap_err();
        assert!(matches!(
            err,
            TransitionRejection::EvidenceForWrongCandidate { .. }
        ));
    }

    #[test]
    fn candidate_behind_commit_watermark_fails_safety_check() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // Watermark is term 2 / lsn 50, but the candidate only applied term 2 lsn 49.
        let req = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            CatalogVersion::initial(),
            ident("CN=node-b"),
            CommitWatermark::new(2, 50),
        )
        .with_evidence(CatchUpEvidence::new(ident("CN=node-b"), 2, 49));
        let err = prepare(&catalog, &req).unwrap_err();
        match err {
            TransitionRejection::SafetyCheckFailed {
                applied_lsn,
                watermark,
                ..
            } => {
                assert_eq!(applied_lsn, 49);
                assert_eq!(watermark, CommitWatermark::new(2, 50));
            }
            other => panic!("expected SafetyCheckFailed, got {other:?}"),
        }
    }

    #[test]
    fn candidate_on_older_term_fails_even_with_higher_lsn() {
        let (catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        // A higher LSN on a stale term does not cover the watermark.
        let req = TransitionRequest::new(
            TransitionKind::Promote,
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            OwnershipEpoch::initial(),
            CatalogVersion::initial(),
            ident("CN=node-b"),
            CommitWatermark::new(3, 10),
        )
        .with_evidence(CatchUpEvidence::new(ident("CN=node-b"), 2, 9999));
        let err = prepare(&catalog, &req).unwrap_err();
        assert!(matches!(err, TransitionRejection::SafetyCheckFailed { .. }));
    }

    #[test]
    fn evidence_on_newer_term_covers_watermark() {
        // A candidate ahead on term covers the watermark regardless of LSN.
        let evidence = CatchUpEvidence::new(ident("CN=node-b"), 5, 0);
        assert!(evidence.covers(CommitWatermark::new(4, 9999)));
    }

    #[test]
    fn rejected_transition_leaves_catalog_unchanged() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b"]);
        let req = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-z");
        assert!(run_transition(&mut catalog, &req).is_err());
        // Ownership and epoch are exactly as before.
        let range = catalog.range(&orders, RangeId::new(1)).unwrap();
        assert_eq!(range.owner(), &ident("CN=node-a"));
        assert_eq!(range.epoch(), OwnershipEpoch::initial());
        assert_eq!(range.version(), CatalogVersion::initial());
    }

    #[test]
    fn second_transition_with_stale_cas_loses() {
        // Two planners race: the first promote wins and moves to v2/epoch2; the
        // second, still holding the v1 CAS, is rejected.
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let first = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-b");
        run_transition(&mut catalog, &first).unwrap();

        // Second planner still thinks node-a owns it at the initial epoch/version.
        let second = request(TransitionKind::Promote, &orders, "CN=node-a", "CN=node-c");
        let err = run_transition(&mut catalog, &second).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::Rejected(TransitionRejection::OwnerMismatch { .. })
        ));
    }
}
