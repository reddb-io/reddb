//! Shared cluster identity and membership model.

pub mod identity;
pub mod join;
pub mod membership;
pub mod ownership;
pub mod routing;
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
pub use routing::{
    RedirectReason, RequestOperation, RouteDecision, RoutedRequest, RoutingHint, RoutingPolicy,
    DEFAULT_MAX_FORWARD_PAYLOAD,
};
pub use topology::{
    ClientTopology, HintOutcome, RefreshOutcome, TopologyRange, TopologySnapshot, TopologyUpdate,
};
