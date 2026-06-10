//! Shared cluster identity and membership model.

pub mod identity;
pub mod join;
pub mod membership;
pub mod ownership;
pub mod ownership_lease;
pub mod ownership_transition;
pub mod routing;
pub mod supervisor;
pub mod topology;

pub use identity::{
    ClusterVoterIdentity, NodeIdentity, NodeIdentityError, ReplicationPeerIdentity,
};
pub use join::{ControlPlaneSnapshot, JoinGrant, JoinRejection, JoinRequest, SeedAuthority};
pub use membership::{
    AdmissionOutcome, BaselineAssessment, ClusterId, ClusterIdError, ClusterMember, MemberKind,
    MembershipCatalog, RESILIENT_DATA_MEMBER_BASELINE,
};
pub use ownership::{
    CatalogError, CatalogVersion, CollectionId, CollectionIdError, OwnershipEpoch,
    PlacementMetadata, RangeBound, RangeBounds, RangeBoundsError, RangeId, RangeOwnership,
    RangeRole, RangeWriteReject, ShardKeyMode, ShardOwnershipCatalog, UpdateOutcome,
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
pub use routing::{
    RedirectReason, RequestOperation, RouteDecision, RoutedRequest, RoutingHint, RoutingPolicy,
    DEFAULT_MAX_FORWARD_PAYLOAD,
};
pub use supervisor::{
    BlockedFailover, BlockedReason, ClusterSignals, ClusterSupervisor, FailoverPlan, HealthClass,
    HealthPolicy, HealthScore, MemberSignals, PlannedPromotion,
};
pub use topology::{
    ClientTopology, HintOutcome, RefreshOutcome, TopologyRange, TopologySnapshot, TopologyUpdate,
};
