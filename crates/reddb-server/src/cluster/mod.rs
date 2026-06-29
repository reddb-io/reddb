//! Shared cluster identity and membership model.

pub mod bootstrap_authority;
pub mod commit_resolution;
pub mod control_plane;
pub mod cross_range;
pub mod drain;
pub mod identity;
pub mod join;
pub mod membership;
pub mod move_range;
pub mod ownership;
pub mod ownership_force;
pub mod ownership_lease;
pub mod ownership_transition;
pub mod placement;
pub mod routing;
pub mod slot;
pub mod supervisor;
pub mod topology;

pub use bootstrap_authority::{
    authorize as authorize_cluster_bootstrap, authorize_vault_bootstrap, is_cluster_shaped,
    plan_vault_bootstrap, AuthBootstrapInput, BootstrapDisposition, VaultBootstrapPlan,
};
pub use commit_resolution::{
    is_local_ack, resolve_commit_policy, CollectionDataModel, CommitPolicyResolution,
    CommitPolicyViolation, FailoverEligibility, GuardrailDisposition, HaIntent, ResolutionSource,
};
pub use control_plane::{
    ControlPlaneConsensus, ControlPlaneEntry, ControlPlaneError, ControlPlaneIndex,
    ControlPlaneTerm, DurableVoteState, MembershipChange, OwnershipTransition,
    SingleNodeControlPlane,
};
pub use cross_range::{
    ConsistentReadLeg, ConsistentReadPlan, ConsistentReadReject, ExactClaimPlan, ExactClaimReject,
    GlobalReadWatermark, KeyTarget, PinnedTarget, RangeParticipant, ReadFanout, ReadFanoutBudget,
    ReadFanoutPolicy, ReadFanoutReject, ReadFanoutTrace, ReadLeg, ResolvedTarget,
    WriteTransactionPlan, WriteTransactionReject, WriterParticipation,
};
pub use drain::{
    drain_status, plan_drain, plan_force_remove, run_drain, run_force_remove, DrainBlock,
    DrainBlockReason, DrainOutcome, DrainPlan, DrainStatus, DrainStep, ForceCapability,
    ForceRemoveAudit, ForceRemoveOrder, ForceRemoveOrderError, ForceRemovePlan, ForceRemoveResult,
    ForcedBlock, ForcedPromotion, OwnedHandoff, RemovalRejection, ReplicaEvacuation,
};
pub use identity::{
    ClusterVoterIdentity, NodeIdentity, NodeIdentityError, ReplicationPeerIdentity,
};
pub use join::{ControlPlaneSnapshot, JoinGrant, JoinRejection, JoinRequest, SeedAuthority};
pub use membership::{
    AdmissionOutcome, BaselineAssessment, ClusterId, ClusterIdError, ClusterMember, MemberKind,
    MemberState, MembershipCatalog, RESILIENT_DATA_MEMBER_BASELINE,
};
pub use move_range::{
    classify_move, recover_interrupted_move, split_range, MoveError, MoveKind, MovePhase,
    MoveRange, MoveRecovery, RangeSplit, SplitError, SplitPolicy, SplitSide,
};
pub use ownership::{
    CatalogError, CatalogVersion, CollectionId, CollectionIdError, OwnershipEpoch,
    PlacementMetadata, RangeBound, RangeBounds, RangeBoundsError, RangeId, RangeOwnership,
    RangeRole, RangeWriteReject, ShardKeyMode, ShardOwnershipCatalog, UpdateOutcome,
};
pub use ownership_force::{
    force_transition, EmptyOperatorReason, ForceDenial, ForceFailure, ForceTransitionCapability,
    ForcedTransitionAudit, ForcedTransitionDisposition, ForcedTransitionRequest, OperatorReason,
};
pub use ownership_lease::{
    admit_durable_write, DurableWriteReject, FenceReason, LeaseFenceRejection, LeasedOwner,
    OwnerWriteMode, OwnershipLease, RangeRequest, SupervisorTerm,
};
pub use ownership_transition::{
    prepare, run_transition, CatchUpEvidence, CommitWatermark, InvalidCandidateReason,
    PreparedTransition, TransitionError, TransitionKind, TransitionOutcome, TransitionRejection,
    TransitionRequest,
};
pub use placement::{
    HotspotRange, MemberCapacity, MoveReason, PlacementPolicy, PlacementSignals, PlannedMove,
    RangeLoad, RebalancePlan, WeightedPlacementPlanner, NEUTRAL_OPERATOR_WEIGHT,
};
pub use routing::{
    RedirectReason, RequestOperation, RouteDecision, RoutedRequest, RoutingHint, RoutingPolicy,
    DEFAULT_MAX_FORWARD_PAYLOAD,
};
pub use slot::{
    hash_shard_key_to_range_key, hash_shard_key_to_slot, HashSlot, HashSlotError,
    PRODUCTION_HASH_SLOT_COUNT,
};
pub use supervisor::{
    BlockedFailover, BlockedReason, ClusterSignals, ClusterSupervisor, FailoverPlan, HealthClass,
    HealthPolicy, HealthScore, MemberSignals, PlannedPromotion,
};
pub use topology::{
    ClientTopology, HintOutcome, RefreshOutcome, TopologyRange, TopologySnapshot, TopologyUpdate,
};
