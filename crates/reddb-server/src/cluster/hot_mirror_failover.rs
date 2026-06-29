//! Operator-invoked hot mirror failover workflow.
//!
//! This is the narrow planned/zero-RPO path: choose a current range replica whose
//! applied log covers the range commit watermark, promote it through the ordinary
//! ownership transition machine, and return evidence that distinguishes this
//! from archive-based recovery.

use super::identity::NodeIdentity;
use super::ownership::{
    CatalogVersion, CollectionId, OwnershipEpoch, RangeId, ShardOwnershipCatalog,
};
use super::ownership_transition::{
    run_transition, CatchUpEvidence, CommitWatermark, TransitionError, TransitionKind,
    TransitionOutcome, TransitionRequest,
};

/// Inputs the planner reads for one range failover.
#[derive(Debug)]
pub struct HotMirrorInputs<'a> {
    catalog: &'a ShardOwnershipCatalog,
    collection: CollectionId,
    range_id: RangeId,
    watermark: CommitWatermark,
    catch_up: Vec<CatchUpEvidence>,
}

impl<'a> HotMirrorInputs<'a> {
    pub fn new(
        catalog: &'a ShardOwnershipCatalog,
        collection: CollectionId,
        range_id: RangeId,
        watermark: CommitWatermark,
    ) -> Self {
        Self {
            catalog,
            collection,
            range_id,
            watermark,
            catch_up: Vec::new(),
        }
    }

    pub fn with_catch_up(mut self, evidence: CatchUpEvidence) -> Self {
        self.catch_up.push(evidence);
        self
    }
}

/// Whether a mirror covers the range commit watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatermarkOutcome {
    Covered,
    Behind { applied_term: u64, applied_lsn: u64 },
}

/// A hot mirror considered by the failover planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotMirrorCandidate {
    pub candidate: NodeIdentity,
    pub watermark_outcome: WatermarkOutcome,
}

/// The range identity carried in operator-facing failover evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotMirrorRangeEvidence {
    pub collection: CollectionId,
    pub range_id: RangeId,
}

/// The ownership epoch boundary crossed by a successful promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotMirrorEpochEvidence {
    pub previous: OwnershipEpoch,
    pub new: OwnershipEpoch,
}

/// Operator-facing result evidence for a successful hot mirror promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotMirrorFailoverEvidence {
    pub workflow: &'static str,
    pub range: HotMirrorRangeEvidence,
    pub previous_owner: NodeIdentity,
    pub promoted_owner: NodeIdentity,
    pub epoch: HotMirrorEpochEvidence,
    pub watermark: CommitWatermark,
    pub watermark_outcome: WatermarkOutcome,
}

impl HotMirrorFailoverEvidence {
    fn from_transition(outcome: TransitionOutcome) -> Self {
        Self {
            workflow: "hot-mirror-zero-rpo",
            range: HotMirrorRangeEvidence {
                collection: outcome.collection,
                range_id: outcome.range_id,
            },
            previous_owner: outcome.previous_owner,
            promoted_owner: outcome.new_owner,
            epoch: HotMirrorEpochEvidence {
                previous: outcome.previous_epoch,
                new: outcome.new_epoch,
            },
            watermark: outcome.watermark,
            watermark_outcome: WatermarkOutcome::Covered,
        }
    }
}

/// A validated plan for one hot mirror failover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotMirrorFailoverPlan {
    collection: CollectionId,
    range_id: RangeId,
    previous_owner: NodeIdentity,
    expected_epoch: OwnershipEpoch,
    expected_version: CatalogVersion,
    watermark: CommitWatermark,
    current_replicas: Vec<NodeIdentity>,
    eligible_candidates: Vec<NodeIdentity>,
    eligible_evidence: Vec<CatchUpEvidence>,
    ineligible_candidates: Vec<HotMirrorCandidate>,
}

impl HotMirrorFailoverPlan {
    pub fn previous_owner(&self) -> &NodeIdentity {
        &self.previous_owner
    }

    pub fn eligible_candidates(&self) -> &[NodeIdentity] {
        &self.eligible_candidates
    }

    pub fn ineligible_candidates(&self) -> &[HotMirrorCandidate] {
        &self.ineligible_candidates
    }

    fn evidence_for(&self, candidate: &NodeIdentity) -> Option<CatchUpEvidence> {
        self.eligible_evidence
            .iter()
            .find(|evidence| evidence.candidate == *candidate)
            .cloned()
    }
}

/// Why planning or executing a hot mirror failover failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotMirrorFailoverError {
    UnknownRange {
        collection: CollectionId,
        range_id: RangeId,
    },
    CandidateNotEligible {
        collection: CollectionId,
        range_id: RangeId,
        candidate: NodeIdentity,
    },
    Transition(TransitionError),
}

impl std::fmt::Display for HotMirrorFailoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRange {
                collection,
                range_id,
            } => write!(
                f,
                "no range {collection}/{range_id} for hot mirror failover"
            ),
            Self::CandidateNotEligible {
                collection,
                range_id,
                candidate,
            } => write!(
                f,
                "candidate {candidate} is not an eligible hot mirror for {collection}/{range_id}"
            ),
            Self::Transition(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for HotMirrorFailoverError {}

/// Identify current replica mirrors that can be promoted without crossing the
/// range commit watermark safety line.
pub fn plan_hot_mirror_failover(
    inputs: &HotMirrorInputs<'_>,
) -> Result<HotMirrorFailoverPlan, HotMirrorFailoverError> {
    let range = inputs
        .catalog
        .range(&inputs.collection, inputs.range_id)
        .ok_or_else(|| HotMirrorFailoverError::UnknownRange {
            collection: inputs.collection.clone(),
            range_id: inputs.range_id,
        })?;

    let mut eligible_candidates = Vec::new();
    let mut eligible_evidence = Vec::new();
    let mut ineligible_candidates = Vec::new();

    for replica in range.replicas() {
        let Some(evidence) = inputs
            .catch_up
            .iter()
            .find(|evidence| evidence.candidate == *replica)
        else {
            ineligible_candidates.push(HotMirrorCandidate {
                candidate: replica.clone(),
                watermark_outcome: WatermarkOutcome::Behind {
                    applied_term: 0,
                    applied_lsn: 0,
                },
            });
            continue;
        };

        if evidence.covers(inputs.watermark) {
            eligible_candidates.push(replica.clone());
            eligible_evidence.push(evidence.clone());
        } else {
            ineligible_candidates.push(HotMirrorCandidate {
                candidate: replica.clone(),
                watermark_outcome: WatermarkOutcome::Behind {
                    applied_term: evidence.applied_term,
                    applied_lsn: evidence.applied_lsn,
                },
            });
        }
    }

    Ok(HotMirrorFailoverPlan {
        collection: inputs.collection.clone(),
        range_id: inputs.range_id,
        previous_owner: range.owner().clone(),
        expected_epoch: range.epoch(),
        expected_version: range.version(),
        watermark: inputs.watermark,
        current_replicas: range.replicas().to_vec(),
        eligible_candidates,
        eligible_evidence,
        ineligible_candidates,
    })
}

/// Promote one eligible hot mirror and return the operator-facing evidence.
pub fn execute_hot_mirror_failover(
    catalog: &mut ShardOwnershipCatalog,
    plan: &HotMirrorFailoverPlan,
    candidate: &NodeIdentity,
) -> Result<HotMirrorFailoverEvidence, HotMirrorFailoverError> {
    let evidence = plan.evidence_for(candidate).ok_or_else(|| {
        HotMirrorFailoverError::CandidateNotEligible {
            collection: plan.collection.clone(),
            range_id: plan.range_id,
            candidate: candidate.clone(),
        }
    })?;
    let request = TransitionRequest::new(
        TransitionKind::Promote,
        plan.collection.clone(),
        plan.range_id,
        plan.previous_owner.clone(),
        plan.expected_epoch,
        plan.expected_version,
        candidate.clone(),
        plan.watermark,
    )
    .with_evidence(evidence)
    .with_replicas(
        plan.current_replicas
            .iter()
            .filter(|replica| *replica != candidate)
            .cloned(),
    );

    run_transition(catalog, &request)
        .map(HotMirrorFailoverEvidence::from_transition)
        .map_err(HotMirrorFailoverError::Transition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::identity::NodeIdentity;
    use crate::cluster::ownership::{
        CollectionId, PlacementMetadata, RangeBounds, RangeId, RangeOwnership, ShardKeyMode,
        ShardOwnershipCatalog,
    };
    use crate::cluster::ownership_lease::{
        admit_durable_write, DurableWriteReject, LeasedOwner, OwnershipLease, SupervisorTerm,
    };
    use crate::cluster::ownership_transition::{CatchUpEvidence, CommitWatermark};

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

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

    #[test]
    fn hot_mirror_failover_promotes_only_a_candidate_covering_the_watermark() {
        let (mut catalog, orders) = catalog_with("CN=node-a", &["CN=node-b", "CN=node-c"]);
        let watermark = CommitWatermark::new(7, 400);
        let inputs = HotMirrorInputs::new(&catalog, orders.clone(), RangeId::new(1), watermark)
            .with_catch_up(CatchUpEvidence::new(ident("CN=node-b"), 7, 400))
            .with_catch_up(CatchUpEvidence::new(ident("CN=node-c"), 7, 399));

        let plan = plan_hot_mirror_failover(&inputs).expect("range is known");
        assert_eq!(plan.previous_owner(), &ident("CN=node-a"));
        assert_eq!(plan.eligible_candidates(), &[ident("CN=node-b")]);
        assert_eq!(
            plan.ineligible_candidates()[0].candidate,
            ident("CN=node-c")
        );

        let stale = execute_hot_mirror_failover(&mut catalog, &plan, &ident("CN=node-c"))
            .expect_err("behind candidate is refused");
        assert!(matches!(
            stale,
            HotMirrorFailoverError::CandidateNotEligible { .. }
        ));
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );

        let old_epoch = catalog.range(&orders, RangeId::new(1)).unwrap().epoch();
        let old_owner_lease = LeasedOwner::with_lease(OwnershipLease::grant(
            SupervisorTerm::genesis(),
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-a"),
            old_epoch,
            0,
            60_000,
        ));

        let evidence =
            execute_hot_mirror_failover(&mut catalog, &plan, &ident("CN=node-b")).unwrap();
        assert_eq!(evidence.range.collection, orders);
        assert_eq!(evidence.previous_owner, ident("CN=node-a"));
        assert_eq!(evidence.promoted_owner, ident("CN=node-b"));
        assert_eq!(evidence.watermark, watermark);
        assert_eq!(evidence.watermark_outcome, WatermarkOutcome::Covered);
        assert!(evidence.epoch.new > evidence.epoch.previous);

        let reject = admit_durable_write(
            &catalog,
            &old_owner_lease,
            &ident("CN=node-a"),
            &orders,
            b"customer-1",
            SupervisorTerm::genesis(),
            1,
        )
        .unwrap_err();
        assert!(matches!(reject, DurableWriteReject::NotOwner { .. }));
    }
}
