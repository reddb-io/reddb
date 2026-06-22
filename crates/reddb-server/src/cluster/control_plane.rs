//! The Cluster Supervisor control-plane consensus boundary (issue #996,
//! PRD #987, ADR 0052).
//!
//! This module pins *the decision*, in types, for how the Cluster Supervisor
//! coordinates: a **Raft-equivalent control-plane consensus layer** carries
//! Supervisor membership, leader election, durable vote/log state, and global
//! ownership-catalog transitions — and **nothing else**. It is the small
//! internal abstraction the HITL decision asked for, so follow-up slices build
//! against a fixed boundary instead of re-opening the protocol choice or
//! picking a consensus library (ADR 0052).
//!
//! ## The one boundary this module exists to enforce
//!
//! **User-data writes never enter the control-plane log.** That is not a
//! convention here — it is structural. [`ControlPlaneEntry`] is the *only*
//! thing that may be appended through [`ControlPlaneConsensus`], and it has no
//! variant that can carry a row, a document, a queue message, or any other
//! user payload. A user write is unrepresentable in this log by construction,
//! so no future slice can accidentally route durable user data through
//! Supervisor consensus and inherit its latency/availability coupling
//! (PRD #987 user story 11; ADR 0030 "without running data payloads through a
//! Raft log").
//!
//! ## What the control-plane log *does* carry
//!
//! Exactly the global control-plane facts that need one safe writer and a
//! totally-ordered, durable, replicated history:
//!
//! * [`ControlPlaneEntry::MembershipChange`] — a member admitted, drained, or
//!   removed from the authorized set ([`super::membership`]).
//! * [`ControlPlaneEntry::OwnershipTransition`] — a fenced, versioned change to
//!   the shard/range ownership catalog (ADR 0037). The Supervisor leader is the
//!   *normal* writer for these (PRD #987 user story 12, the glossary's "Shard
//!   ownership catalog").
//! * [`ControlPlaneEntry::LeaderConfiguration`] — a record that a Supervisor
//!   term elected a leader, so the elected-term history is itself durable.
//!
//! ## Relationship to the existing election primitives
//!
//! The Raft-equivalent *vote-safety* mechanics already exist for primary
//! election in [`crate::replication::election`] — term bump, durable last-vote,
//! majority-quorum, "no two leaders in a term". The Cluster Supervisor reuses
//! those same mechanics for its own leader election (ADR 0030's decoupled
//! supervisor). What is *new* here, and what this module names, is the
//! **control-plane log**: the replicated, ordered entry stream the elected
//! leader appends to. The trait keeps the concrete engine — single-node now, a
//! full replicated log later — behind one seam.
//!
//! Everything in this module is a pure data model plus one trait, with no I/O,
//! so the boundary is exercised deterministically. A minimal in-memory engine
//! ([`SingleNodeControlPlane`]) implements the trait so the leader-only-append
//! and no-user-data invariants are tested, not merely asserted in prose.

use super::identity::ClusterVoterIdentity;

/// A Supervisor election term — a strictly monotonic generation that fences a
/// stale leader. A new term is minted by each election round, exactly as in
/// [`crate::replication::election`]; the control-plane log stamps every entry
/// with the term that produced it so a deposed leader's entries are detectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ControlPlaneTerm(pub u64);

impl ControlPlaneTerm {
    /// The genesis term, before any election has run.
    pub const GENESIS: ControlPlaneTerm = ControlPlaneTerm(0);

    /// The next term a candidate stands for. Election bumps the term by one.
    pub fn next(self) -> ControlPlaneTerm {
        ControlPlaneTerm(self.0 + 1)
    }
}

/// A position in the control-plane log. The log is append-only and totally
/// ordered, so an index uniquely names one committed control-plane fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControlPlaneIndex(pub u64);

/// The durable per-node vote state every voting member must persist *before*
/// acknowledging a vote, so a member that crashes and restarts mid-term cannot
/// double-vote and split a term (ADR 0030: "the supervisor needs durable
/// per-node vote state (last-vote) to prevent double-voting across restarts").
///
/// This is the control-plane analogue of
/// [`crate::replication::election::LastVote`]; it is named here as a *required*
/// part of the consensus boundary so a follow-up slice cannot ship a leader
/// election without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableVoteState {
    /// The highest term this member has voted in.
    pub term: ControlPlaneTerm,
    /// Who this member granted its vote to in `term`, if anyone. `None` means
    /// the member has seen `term` but not yet voted in it.
    pub voted_for: Option<ClusterVoterIdentity>,
}

impl DurableVoteState {
    /// The vote state of a member that has never voted.
    pub fn initial() -> Self {
        Self {
            term: ControlPlaneTerm::GENESIS,
            voted_for: None,
        }
    }
}

/// The shard/range ownership-catalog change recorded by a single control-plane
/// log entry. This is deliberately the *fact* of a fenced transition, not the
/// full catalog schema (that lives with ADR 0037 / ADR 0045 follow-ups): the
/// boundary this module fixes is only that ownership transitions are
/// control-plane log entries written by the leader, with an ownership epoch
/// that fences the old owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipTransition {
    /// The range whose ownership is changing, named opaquely here so this
    /// boundary type does not pin the range-id encoding the catalog slice owns.
    pub range: String,
    /// The new owner after the transition.
    pub new_owner: ClusterVoterIdentity,
    /// The ownership epoch the transition bumps to. A monotonic epoch is what
    /// fences a stale owner that reappears (ADR 0037 "Fencing is enforced below
    /// routing"); recording it in the consensus log makes the fence durable.
    pub ownership_epoch: u64,
}

/// A change to the authorized-member set, recorded durably so membership is
/// agreed control-plane state rather than each node's local guess
/// ([`super::membership`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipChange {
    /// A member was admitted to the cluster (after the [`super::join`]
    /// handshake authorized it).
    Admit(ClusterVoterIdentity),
    /// A member finished draining and was removed from the authorized set.
    Remove(ClusterVoterIdentity),
}

/// The complete, closed set of things that may be appended to the Cluster
/// Supervisor control-plane log.
///
/// There is **no user-data variant on purpose**. The absence is the enforcement
/// mechanism for the central decision (ADR 0052): durable user writes cannot be
/// expressed as a control-plane entry, so they cannot be routed through, gated
/// by, or made to wait on Supervisor consensus. Adding a user-payload variant
/// here would be a decision reversal and must go through a new ADR, not a code
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneEntry {
    /// A member admitted to or removed from the cluster.
    MembershipChange(MembershipChange),
    /// A fenced, versioned shard/range ownership-catalog transition — the
    /// Supervisor leader's normal write.
    OwnershipTransition(OwnershipTransition),
    /// A record that `term` elected `leader`, keeping the elected-term history
    /// durable alongside the entries that term produced.
    LeaderConfiguration {
        /// The Supervisor term that elected the leader.
        term: ControlPlaneTerm,
        /// The member elected leader for `term`.
        leader: ClusterVoterIdentity,
    },
}

/// Why an attempt to append to the control-plane log was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneError {
    /// The caller is not the current Supervisor leader. Only the elected leader
    /// is the normal writer for control-plane entries (PRD #987 user story 12);
    /// a follower that tries to append is told who the leader is (if known) so
    /// it can forward.
    NotLeader {
        /// The current leader, if this node knows one.
        leader: Option<ClusterVoterIdentity>,
    },
}

impl std::fmt::Display for ControlPlaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotLeader { leader: Some(l) } => {
                write!(f, "not the control-plane leader; current leader is {l}")
            }
            Self::NotLeader { leader: None } => {
                write!(f, "not the control-plane leader; no leader currently known")
            }
        }
    }
}

impl std::error::Error for ControlPlaneError {}

/// The small internal abstraction over the Cluster Supervisor control-plane
/// consensus engine.
///
/// This trait is the seam the HITL decision asked for: follow-up slices depend
/// on *this*, not on a concrete Raft library or hand-rolled protocol. The first
/// implementation may be a degenerate single-node engine
/// ([`SingleNodeControlPlane`]); a later slice may back it with a fully
/// replicated, persisted log. Neither change reaches the callers.
///
/// The trait deliberately exposes only what the boundary needs:
///
/// * read the current [`ControlPlaneTerm`] and the elected [`leader`];
/// * append a [`ControlPlaneEntry`] — **leader-only**, which is how "the
///   Supervisor leader is the normal writer for ownership transitions" is
///   enforced rather than documented; and
/// * read the durable [`DurableVoteState`], so the election safety obligation
///   is part of the contract.
///
/// [`leader`]: ControlPlaneConsensus::leader
pub trait ControlPlaneConsensus {
    /// The current Supervisor election term.
    fn current_term(&self) -> ControlPlaneTerm;

    /// The current Supervisor leader, if one is elected for [`current_term`].
    ///
    /// [`current_term`]: ControlPlaneConsensus::current_term
    fn leader(&self) -> Option<ClusterVoterIdentity>;

    /// Is *this* node the current Supervisor leader?
    fn is_leader(&self) -> bool;

    /// The durable per-node vote state, persisted before acknowledging a vote.
    fn durable_vote(&self) -> DurableVoteState;

    /// The index of the highest committed entry, or `None` if the log is empty.
    fn commit_index(&self) -> Option<ControlPlaneIndex>;

    /// Append a control-plane entry as the leader.
    ///
    /// Returns the committed [`ControlPlaneIndex`] on success, or
    /// [`ControlPlaneError::NotLeader`] if this node is not the leader — a
    /// follower must forward the request to the leader rather than write
    /// locally. By construction, the `entry` cannot carry user data.
    fn append(&mut self, entry: ControlPlaneEntry) -> Result<ControlPlaneIndex, ControlPlaneError>;
}

/// A minimal single-node control-plane engine: this node is the sole voter and
/// therefore the leader of term 1. It exists to (a) give the first cut a usable
/// implementation of the boundary, and (b) make the leader-only-append and
/// no-user-data invariants executable tests rather than prose.
///
/// It is intentionally *not* the replicated engine — that is a later slice.
/// What matters is that the later engine swaps in behind [`ControlPlaneConsensus`]
/// without touching callers.
#[derive(Debug)]
pub struct SingleNodeControlPlane {
    identity: ClusterVoterIdentity,
    term: ControlPlaneTerm,
    is_leader: bool,
    vote: DurableVoteState,
    log: Vec<ControlPlaneEntry>,
}

impl SingleNodeControlPlane {
    /// A single-node control plane that has elected `identity` as the leader of
    /// term 1, voting for itself. This is the trivial-but-correct quorum: a
    /// one-member majority.
    pub fn bootstrap_leader(identity: ClusterVoterIdentity) -> Self {
        let term = ControlPlaneTerm::GENESIS.next();
        Self {
            term,
            is_leader: true,
            vote: DurableVoteState {
                term,
                voted_for: Some(identity.clone()),
            },
            identity,
            log: Vec::new(),
        }
    }

    /// The entries committed so far, in log order.
    pub fn entries(&self) -> &[ControlPlaneEntry] {
        &self.log
    }
}

impl ControlPlaneConsensus for SingleNodeControlPlane {
    fn current_term(&self) -> ControlPlaneTerm {
        self.term
    }

    fn leader(&self) -> Option<ClusterVoterIdentity> {
        self.is_leader.then(|| self.identity.clone())
    }

    fn is_leader(&self) -> bool {
        self.is_leader
    }

    fn durable_vote(&self) -> DurableVoteState {
        self.vote.clone()
    }

    fn commit_index(&self) -> Option<ControlPlaneIndex> {
        self.log
            .len()
            .checked_sub(1)
            .map(|i| ControlPlaneIndex(i as u64))
    }

    fn append(&mut self, entry: ControlPlaneEntry) -> Result<ControlPlaneIndex, ControlPlaneError> {
        if !self.is_leader {
            return Err(ControlPlaneError::NotLeader {
                leader: self.leader(),
            });
        }
        self.log.push(entry);
        Ok(ControlPlaneIndex(self.log.len() as u64 - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voter(cn: &str) -> ClusterVoterIdentity {
        ClusterVoterIdentity::from_certificate_subject(cn).unwrap()
    }

    #[test]
    fn bootstrap_leader_elects_itself_in_term_one() {
        let id = voter("CN=node-a");
        let cp = SingleNodeControlPlane::bootstrap_leader(id.clone());

        assert_eq!(cp.current_term(), ControlPlaneTerm(1));
        assert!(cp.is_leader());
        assert_eq!(cp.leader(), Some(id.clone()));
        // The durable vote is recorded for the elected term, for itself.
        assert_eq!(
            cp.durable_vote(),
            DurableVoteState {
                term: ControlPlaneTerm(1),
                voted_for: Some(id),
            }
        );
        // An empty log has no commit index yet.
        assert_eq!(cp.commit_index(), None);
    }

    #[test]
    fn leader_appends_ownership_transition_and_commit_index_advances() {
        let mut cp = SingleNodeControlPlane::bootstrap_leader(voter("CN=node-a"));

        let idx = cp
            .append(ControlPlaneEntry::OwnershipTransition(
                OwnershipTransition {
                    range: "users:[0,1000)".to_string(),
                    new_owner: voter("CN=node-b"),
                    ownership_epoch: 7,
                },
            ))
            .expect("leader may append");

        assert_eq!(idx, ControlPlaneIndex(0));
        assert_eq!(cp.commit_index(), Some(ControlPlaneIndex(0)));
        assert_eq!(cp.entries().len(), 1);
    }

    #[test]
    fn membership_and_leader_config_entries_are_ordered() {
        let mut cp = SingleNodeControlPlane::bootstrap_leader(voter("CN=node-a"));

        let first = cp
            .append(ControlPlaneEntry::LeaderConfiguration {
                term: ControlPlaneTerm(1),
                leader: voter("CN=node-a"),
            })
            .unwrap();
        let second = cp
            .append(ControlPlaneEntry::MembershipChange(
                MembershipChange::Admit(voter("CN=node-b")),
            ))
            .unwrap();

        assert!(second > first);
        assert_eq!(cp.commit_index(), Some(second));
    }

    #[test]
    fn a_follower_may_not_append() {
        // Force the non-leader path: a node that is not the leader must refuse
        // to write the control-plane log and point at the leader instead.
        let mut cp = SingleNodeControlPlane::bootstrap_leader(voter("CN=node-a"));
        cp.is_leader = false;

        let err = cp
            .append(ControlPlaneEntry::MembershipChange(
                MembershipChange::Remove(voter("CN=node-b")),
            ))
            .expect_err("a follower must not append");

        assert_eq!(err, ControlPlaneError::NotLeader { leader: None });
    }
}
