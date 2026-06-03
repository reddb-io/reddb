//! Witness runtime profile (issue #836, PRD #819, ADR 0030).
//!
//! A **witness** is a node that runs *only* the control-plane supervisor —
//! the vote path of the [election core](super::election) — and boots **no
//! data plane** (no storage engine, no WAL, no replication streaming). It
//! holds no data and can never be promoted to primary, but its vote counts
//! toward the election quorum. This makes `2 data nodes + 1 witness` a valid
//! HA shape (the Mongo "arbiter" idea), so an operator gets automatic
//! failover without standing up a third *data* replica.
//!
//! ADR 0030 fixes the shape: "*The supervisor is therefore a module every
//! node runs; a witness is a node that runs only that module.*" and
//! "*Witness members require a build/runtime profile that excludes the data
//! plane.*" This module is that profile.
//!
//! ## What a witness is, structurally
//!
//! * [`RuntimeProfile`] — the boot-time choice between a data-bearing node
//!   ([`RuntimeProfile::Data`], supervisor + data plane) and a witness
//!   ([`RuntimeProfile::Witness`], supervisor only). `boots_data_plane()` is
//!   the one bit the boot pipeline branches on.
//! * [`WitnessSupervisor`] — a booted witness. It is exactly a durable
//!   [`Voter`](super::election::Voter) plus the node's shared
//!   [`NodeIdentity`](crate::cluster::NodeIdentity); there is, by
//!   construction, nothing else. The absence of a data-plane field *is* the
//!   guarantee — a witness cannot accidentally serve a read or accept a
//!   write because it holds no engine to do so.
//!
//! ## Shared identity, not a second namespace
//!
//! A witness authenticates with the **same per-node mTLS identity** a data
//! member uses: [`NodeIdentity`](crate::cluster::NodeIdentity) is the
//! validated X.509 subject of the node certificate, and the same type backs
//! both [`ReplicationPeerIdentity`](crate::cluster::ReplicationPeerIdentity)
//! and [`ClusterVoterIdentity`](crate::cluster::ClusterVoterIdentity). The
//! witness's membership id is that identity's subject, so its votes land in
//! the same identity namespace as every data member's acks — a witness is
//! not a second-class peer with a parallel auth path.

use std::path::PathBuf;

use crate::cluster::NodeIdentity;

use super::election::{
    FileLastVoteStore, LastVoteError, LastVoteStore, Member, MemberKind, VoteDecision, VoteRequest,
    Voter,
};

/// Which planes a node boots.
///
/// Every node runs the control-plane supervisor (the vote path). The profile
/// decides whether the *data plane* — storage engine, WAL, replication
/// streaming — is constructed alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProfile {
    /// A data-bearing node: supervisor **and** data plane. Holds data,
    /// streams WAL, and can be promoted to primary.
    Data,
    /// A witness: the supervisor / vote path **only**. Holds no data, boots
    /// no data plane, and can never be promoted (ADR 0030).
    Witness,
}

impl RuntimeProfile {
    /// Does this profile boot the data plane (storage engine + WAL +
    /// replication streaming)? Only [`RuntimeProfile::Data`] does — a witness
    /// is supervisor-only.
    pub fn boots_data_plane(self) -> bool {
        matches!(self, RuntimeProfile::Data)
    }

    /// The supervisor module runs on *every* profile — that is the whole
    /// point of the decoupled control plane (ADR 0030). A witness is the
    /// degenerate node that runs nothing else.
    pub fn boots_supervisor(self) -> bool {
        true
    }

    /// The membership kind this profile presents to the election quorum.
    pub fn member_kind(self) -> MemberKind {
        match self {
            RuntimeProfile::Data => MemberKind::Data,
            RuntimeProfile::Witness => MemberKind::Witness,
        }
    }
}

/// A booted witness node: the control-plane supervisor with no data plane.
///
/// A witness is a [`Voter`] over a durable last-vote store plus the node's
/// shared [`NodeIdentity`] — and nothing else. There is intentionally no
/// engine, no WAL, and no replication handle on this struct: a witness
/// *cannot* serve data because it holds none.
///
/// The store type is generic so production uses the durable
/// [`FileLastVoteStore`] (ADR 0030: "the supervisor needs durable per-node
/// vote state to prevent double-voting across restarts") while tests use an
/// in-memory store.
pub struct WitnessSupervisor<S: LastVoteStore> {
    identity: NodeIdentity,
    voter: Voter<S>,
}

impl<S: LastVoteStore> WitnessSupervisor<S> {
    /// Boot a witness supervisor over `store`, identified by the shared
    /// per-node `identity`. The voter id is the identity's certificate
    /// subject, so the witness votes under the same identity a data member
    /// would replicate under.
    pub fn new(identity: NodeIdentity, store: S) -> Self {
        let voter = Voter::new(identity.as_str(), store);
        Self { identity, voter }
    }

    /// A witness always runs the witness profile.
    pub fn profile(&self) -> RuntimeProfile {
        RuntimeProfile::Witness
    }

    /// A witness never boots a data plane — invariant by construction, stated
    /// here so callers (and the boot pipeline) can assert it without reaching
    /// into the profile.
    pub fn boots_data_plane(&self) -> bool {
        false
    }

    /// The shared per-node identity this witness authenticates with — the
    /// same [`NodeIdentity`](crate::cluster::NodeIdentity) type a data member
    /// presents over mTLS.
    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    /// This witness's entry in the supervisor's membership view: a vote-only
    /// [`MemberKind::Witness`], always [`VotingState::Voting`](super::election::VotingState::Voting).
    /// It counts toward quorum but is never electable.
    pub fn member(&self) -> Member {
        Member::witness(self.identity.as_str())
    }

    /// Consider a candidate's vote request against the current commit
    /// watermark — the only control-plane action a witness performs. The
    /// watermark rule and the durable double-vote guard live in the
    /// [`Voter`], so a witness applies the exact same safety rule a data
    /// voter does.
    pub fn consider_vote(
        &self,
        req: &VoteRequest,
        commit_watermark: u64,
    ) -> Result<VoteDecision, LastVoteError> {
        self.voter.consider(req, commit_watermark)
    }

    /// The highest term this witness has durably recorded.
    pub fn current_term(&self) -> Result<u64, LastVoteError> {
        self.voter.current_term()
    }
}

impl WitnessSupervisor<FileLastVoteStore> {
    /// Boot a witness with a durable, on-disk last-vote store at
    /// `last_vote_path` — the production constructor. Survives a restart so a
    /// witness that crashes mid-term never double-votes (ADR 0030).
    pub fn with_durable_store(identity: NodeIdentity, last_vote_path: impl Into<PathBuf>) -> Self {
        Self::new(identity, FileLastVoteStore::new(last_vote_path))
    }
}

#[cfg(test)]
mod tests;
