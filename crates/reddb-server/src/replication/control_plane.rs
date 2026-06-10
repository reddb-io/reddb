//! Control-plane consensus seam (issue #996, parent #987, ADR 0052).
//!
//! The Cluster Supervisor coordinates membership, leader election, and
//! shard/range ownership through a **Raft-equivalent control-plane consensus
//! layer** — and *only* the control plane. [ADR 0052] fixes the boundary; this
//! module is the small internal abstraction that boundary lives behind, so the
//! follow-up implementation slices (the replicated log, its durable store, its
//! snapshot/compaction) target a named seam instead of picking a consensus
//! library or inventing protocol semantics.
//!
//! ## What goes through this layer — and what never does
//!
//! The control-plane log carries exactly two kinds of entry:
//!
//! * a **membership change** — admission, removal, or role change of a cluster
//!   member; and
//! * an **ownership-catalog transition** — a fenced, versioned move / split /
//!   merge / promote of a shard/range (ADR 0037).
//!
//! [`ControlPlaneEntry`] is *closed* over exactly these two: there is, by
//! construction, **no user-data variant**. User-data writes are never recorded
//! in, ordered by, or gated on the control-plane log — they flow through the
//! data plane (WAL → logical stream → replicas, ADR 0030/0044) under per-range
//! ownership and commit policy. The control plane decides *who may write a
//! range and where it lives*; it does not carry *what* is written. The two logs
//! are physically separate and a user write touches exactly one of them. This
//! is the central line ADR 0030 drew ("the term/epoch and vote-safety ideas
//! without running data payloads through a Raft log") made concrete and
//! type-enforced.
//!
//! ## Relationship to the election core
//!
//! The election half of this protocol already exists: term-based, quorum-gated
//! election with a durable last-vote in [`super::election`] (issue #834), and
//! the vote-only [`super::witness`] profile (issue #836). This seam names the
//! *log* half — append + commit of control-plane entries — and ties it to the
//! same role/term so the whole forms one Raft-equivalent layer. The concrete
//! engine behind [`ControlPlaneConsensus`] (an embedded Raft crate, the
//! election core extended with a replicated log, or another quorum protocol) is
//! an implementation detail; swapping it must not change the boundary above.
//!
//! ## Durable state requirement
//!
//! An implementation must persist, fsync-ordered, before acknowledging a vote
//! grant or reporting an entry committed: the current term, `voted_for` for
//! that term (the existing [`super::election::LastVoteStore`]), and the accepted
//! control-plane log entries plus the highest committed index. Election safety
//! (one leader per term) and log safety (a committed entry is never lost) both
//! depend on it (ADR 0052, safety properties 1–3).
//!
//! [ADR 0052]: ../../../../.red/adr/0052-cluster-supervisor-control-plane-consensus.md

/// Stable identity of a cluster member, matching the election membership id
/// ([`super::election::Member::id`]) and the replica/ack id namespace.
pub type MemberId = String;

/// Position of an entry in the control-plane log. Monotonic per term-history;
/// an entry is durable once its index is at or below the committed index of a
/// quorum.
pub type ControlPlaneLogIndex = u64;

/// This node's role in the control-plane consensus layer.
///
/// Mirrors the Raft-equivalent roles. A node that holds data may take any role;
/// a [witness](super::witness) participates as a [`Follower`](Self::Follower) or
/// [`Candidate`](Self::Candidate) but never leads a data range — its leadership
/// is over control-plane state only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlaneRole {
    /// Currently leading the control-plane log; the normal writer of ownership
    /// catalog transitions (ADR 0037, ADR 0052).
    Leader,
    /// Following the current leader; applies committed entries, votes per the
    /// durable last-vote rule.
    Follower,
    /// Standing in an election for a new term; not yet a leader.
    Candidate,
}

/// The kind of a [`ControlPlaneEntry`], without its payload.
///
/// Exhaustive over everything the control-plane log may carry. The deliberate
/// absence of a user-data kind is the type-level half of ADR 0052's
/// data/control isolation property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlaneEntryKind {
    /// Admission, removal, or role change of a cluster member.
    MembershipChange,
    /// A fenced, versioned shard/range ownership catalog transition (ADR 0037).
    OwnershipTransition,
}

/// Opaque, already-encoded body of a control-plane entry.
///
/// The seam stays agnostic to *how* a membership change or an ownership
/// transition is encoded: the slice that implements that entry owns its wire
/// shape. The seam only guarantees the body is one of the two control-plane
/// kinds — never user data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlanePayload(pub Vec<u8>);

impl ControlPlanePayload {
    /// Borrow the encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A single entry in the control-plane log.
///
/// Closed over exactly the two control-plane concerns (see
/// [`ControlPlaneEntryKind`]). There is no constructor for a user-data entry,
/// so the data/control boundary cannot be crossed by appending to this log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneEntry {
    /// Admission, removal, or role change of a cluster member.
    MembershipChange(ControlPlanePayload),
    /// A fenced, versioned shard/range ownership catalog transition (ADR 0037),
    /// normally proposed by the Supervisor leader.
    OwnershipTransition(ControlPlanePayload),
}

impl ControlPlaneEntry {
    /// The kind of this entry, without its payload.
    #[must_use]
    pub fn kind(&self) -> ControlPlaneEntryKind {
        match self {
            Self::MembershipChange(_) => ControlPlaneEntryKind::MembershipChange,
            Self::OwnershipTransition(_) => ControlPlaneEntryKind::OwnershipTransition,
        }
    }

    /// Borrow the entry's opaque payload.
    #[must_use]
    pub fn payload(&self) -> &ControlPlanePayload {
        match self {
            Self::MembershipChange(p) | Self::OwnershipTransition(p) => p,
        }
    }
}

/// Why a [`ControlPlaneConsensus::propose`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposeRefusal {
    /// This node is not the current leader. Only the leader is the normal writer
    /// of control-plane entries (ADR 0052); the caller should route to
    /// [`ControlPlaneConsensus::leader`] or retry after the next election.
    NotLeader {
        /// The leader this node currently believes in, if any.
        leader: Option<MemberId>,
    },
    /// The node lost the control-plane quorum and cannot commit new entries
    /// until quorum/lease authority is restored (owner self-fence, ADR 0037).
    NoQuorum,
}

/// The Cluster Supervisor's control-plane consensus seam.
///
/// A follow-up slice implements this over a concrete Raft-equivalent engine.
/// Callers — the rebalancer, the join/drain flows, the ownership-transition
/// machinery — depend on this trait and the ADR 0052 boundaries, never on the
/// engine. The contract is intentionally narrow: read the current
/// role/term/leader and committed index, and (leader-only) propose a
/// control-plane entry.
///
/// Implementations must uphold ADR 0052's safety properties: one leader per
/// term, committed entries never lost, no double-vote across restart, and the
/// data/control isolation that [`ControlPlaneEntry`] already enforces at the
/// type level.
pub trait ControlPlaneConsensus {
    /// This node's current control-plane role.
    fn role(&self) -> ControlPlaneRole;

    /// The current control-plane term.
    fn term(&self) -> u64;

    /// The member this node currently believes is leader, if any.
    fn leader(&self) -> Option<MemberId>;

    /// Highest control-plane log index known committed (durable to a quorum).
    fn committed_index(&self) -> ControlPlaneLogIndex;

    /// Whether this node is the current leader — the normal writer of
    /// ownership-catalog transitions (ADR 0052).
    fn is_leader(&self) -> bool {
        self.role() == ControlPlaneRole::Leader
    }

    /// Append a control-plane entry to the replicated log.
    ///
    /// Leader-only. Returns the index the entry will occupy once committed by a
    /// quorum, or a [`ProposeRefusal`] if this node is not the leader or has
    /// lost quorum. The returned index is *assigned*, not yet committed; callers
    /// that need durability wait for [`committed_index`](Self::committed_index)
    /// to reach it.
    fn propose(&mut self, entry: ControlPlaneEntry)
        -> Result<ControlPlaneLogIndex, ProposeRefusal>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> ControlPlanePayload {
        ControlPlanePayload(vec![1, 2, 3])
    }

    #[test]
    fn entry_kind_maps_each_variant() {
        assert_eq!(
            ControlPlaneEntry::MembershipChange(payload()).kind(),
            ControlPlaneEntryKind::MembershipChange,
        );
        assert_eq!(
            ControlPlaneEntry::OwnershipTransition(payload()).kind(),
            ControlPlaneEntryKind::OwnershipTransition,
        );
    }

    #[test]
    fn entry_payload_is_accessible_for_both_kinds() {
        let m = ControlPlaneEntry::MembershipChange(payload());
        let o = ControlPlaneEntry::OwnershipTransition(payload());
        assert_eq!(m.payload().as_bytes(), &[1, 2, 3]);
        assert_eq!(o.payload().as_bytes(), &[1, 2, 3]);
    }

    /// The control-plane entry set is closed over exactly the two control-plane
    /// concerns — there is no user-data variant to match. This exhaustive match
    /// fails to compile if a non-control-plane variant is ever added, which is
    /// the type-level enforcement of ADR 0052's data/control isolation.
    #[test]
    fn entry_set_is_closed_over_control_plane_only() {
        let kinds = [
            ControlPlaneEntryKind::MembershipChange,
            ControlPlaneEntryKind::OwnershipTransition,
        ];
        for kind in kinds {
            match kind {
                ControlPlaneEntryKind::MembershipChange
                | ControlPlaneEntryKind::OwnershipTransition => {}
            }
        }
    }

    /// A minimal seam implementation, proving the trait expresses leader-only
    /// `propose` and the role/term/leader/committed reads a slice will rely on.
    struct FakeConsensus {
        role: ControlPlaneRole,
        term: u64,
        leader: Option<MemberId>,
        committed: ControlPlaneLogIndex,
        next_index: ControlPlaneLogIndex,
        has_quorum: bool,
    }

    impl ControlPlaneConsensus for FakeConsensus {
        fn role(&self) -> ControlPlaneRole {
            self.role
        }
        fn term(&self) -> u64 {
            self.term
        }
        fn leader(&self) -> Option<MemberId> {
            self.leader.clone()
        }
        fn committed_index(&self) -> ControlPlaneLogIndex {
            self.committed
        }
        fn propose(
            &mut self,
            _entry: ControlPlaneEntry,
        ) -> Result<ControlPlaneLogIndex, ProposeRefusal> {
            if self.role != ControlPlaneRole::Leader {
                return Err(ProposeRefusal::NotLeader {
                    leader: self.leader.clone(),
                });
            }
            if !self.has_quorum {
                return Err(ProposeRefusal::NoQuorum);
            }
            let idx = self.next_index;
            self.next_index += 1;
            Ok(idx)
        }
    }

    #[test]
    fn follower_propose_is_refused_with_leader_hint() {
        let mut node = FakeConsensus {
            role: ControlPlaneRole::Follower,
            term: 7,
            leader: Some("n1".to_string()),
            committed: 42,
            next_index: 43,
            has_quorum: true,
        };
        assert!(!node.is_leader());
        assert_eq!(node.term(), 7);
        assert_eq!(node.committed_index(), 42);
        let refusal = node
            .propose(ControlPlaneEntry::OwnershipTransition(payload()))
            .unwrap_err();
        assert_eq!(
            refusal,
            ProposeRefusal::NotLeader {
                leader: Some("n1".to_string())
            }
        );
    }

    #[test]
    fn leader_propose_assigns_increasing_indexes() {
        let mut node = FakeConsensus {
            role: ControlPlaneRole::Leader,
            term: 9,
            leader: Some("self".to_string()),
            committed: 10,
            next_index: 11,
            has_quorum: true,
        };
        assert!(node.is_leader());
        let a = node
            .propose(ControlPlaneEntry::OwnershipTransition(payload()))
            .unwrap();
        let b = node
            .propose(ControlPlaneEntry::MembershipChange(payload()))
            .unwrap();
        assert_eq!(a, 11);
        assert_eq!(b, 12);
    }

    #[test]
    fn leader_without_quorum_self_fences() {
        let mut node = FakeConsensus {
            role: ControlPlaneRole::Leader,
            term: 9,
            leader: Some("self".to_string()),
            committed: 10,
            next_index: 11,
            has_quorum: false,
        };
        let refusal = node
            .propose(ControlPlaneEntry::MembershipChange(payload()))
            .unwrap_err();
        assert_eq!(refusal, ProposeRefusal::NoQuorum);
    }
}
