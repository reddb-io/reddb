//! Shared cluster identity model.

pub mod identity;

pub use identity::{
    ClusterVoterIdentity, NodeIdentity, NodeIdentityError, ReplicationPeerIdentity,
};
