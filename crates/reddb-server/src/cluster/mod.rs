//! Shared cluster identity and membership model.

pub mod identity;
pub mod join;
pub mod membership;
pub mod ownership;

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
    ShardKeyMode, ShardOwnershipCatalog, UpdateOutcome,
};
