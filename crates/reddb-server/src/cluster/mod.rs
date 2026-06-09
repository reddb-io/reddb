//! Shared cluster identity and membership model.

pub mod identity;
pub mod join;
pub mod membership;

pub use identity::{
    ClusterVoterIdentity, NodeIdentity, NodeIdentityError, ReplicationPeerIdentity,
};
pub use join::{ControlPlaneSnapshot, JoinGrant, JoinRejection, JoinRequest, SeedAuthority};
pub use membership::{
    AdmissionOutcome, BaselineAssessment, ClusterId, ClusterIdError, ClusterMember, MemberKind,
    MembershipCatalog, RESILIENT_DATA_MEMBER_BASELINE,
};
