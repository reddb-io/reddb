# ADR 0052 — Cluster Supervisor Control-Plane Consensus

Status: accepted
Date: 2026-06-10

Resolves issue #996 (parent #987). Extends [ADR 0030](0030-replication-consistency-and-failover-model.md)
(replication consistency and failover model) and [ADR 0037](0037-shard-range-ownership-catalog.md)
(shard/range ownership catalog).

The Cluster Supervisor is RedDB's native control-plane module: it manages
membership, health, failover, and shard/range ownership while staying
decoupled from the data-plane write path (see the
[clustering glossary](../context/clustering.md)). ADR 0030 fixed *who decides
the leader* and *what a write acknowledgement guarantees*. ADR 0037 fixed that
ownership is versioned catalog state mutated through fenced transitions. What
neither pinned down — and what this ADR decides — is the **concrete
control-plane consensus approach**: how Supervisor membership, leader election,
durable vote/log state, and ownership-catalog transitions are agreed and
persisted, so follow-up implementation slices can proceed without re-opening the
protocol choice or selecting a consensus library.

## Decision

**A Raft-equivalent consensus layer governs the control plane only.** Supervisor
membership, leader election, durable vote/log state, and global ownership
catalog transitions are agreed through a single replicated control-plane log
with Raft-equivalent safety: a term-based election with a strict-majority quorum,
a durable per-node last-vote so no node double-votes across a restart, and an
append-only log whose entries are committed once replicated to a quorum. This is
the layer the existing election core
([`replication::election`](../../crates/reddb-server/src/replication/election.rs),
issue #834) and witness profile (issue #836) already begin; this ADR names the
log half of the same protocol and fixes its boundaries.

**User-data writes stay outside the control-plane consensus log.** The
control-plane log carries *only* control-plane entries — membership changes and
ownership-catalog transitions. User-data writes are never recorded in it and are
never gated by reaching a control-plane commit index. User data continues to
flow through the data plane (WAL → logical replication stream → replicas, ADR
0030/0044) under per-range ownership and commit policy. The control plane decides
*who may write a range and where it lives*; it does not carry *what* is written.
This is the central line ADR 0030 drew ("we take the term/epoch and vote-safety
ideas without running data payloads through a Raft log") made concrete: the two
logs are physically separate and a user write touches exactly one of them.

**Durable vote/log state is a hard requirement for Supervisor leader election.**
Every voting member persists, on disk and fsync-ordered *before* the grant is
acknowledged: (1) the current term, (2) `voted_for` for that term (the existing
[`LastVoteStore`](../../crates/reddb-server/src/replication/election.rs)), and
(3) the control-plane log entries it has accepted plus the highest committed
index. A member that crashes and restarts mid-term must recover this state and
must not double-vote or lose a committed control-plane entry. Election safety
(at most one leader per term) and log safety (a committed entry is never lost)
both depend on this durability and are not optional for an HA deployment.

**The elected Supervisor leader is the normal writer for ownership-catalog
transitions.** A shard/range ownership transition (move, split, merge, promote;
ADR 0037) is, in the normal path, proposed and appended to the control-plane log
by the current Supervisor leader and applied once committed. The leader is the
single normal writer of catalog state; followers learn transitions by applying
the committed log. This keeps ADR 0037's "ownership changes are transitions, not
arbitrary row edits" intact and gives those transitions the same fenced,
versioned, audited machinery. Forced/administrative recovery transitions remain
the documented exception (ADR 0037), proceeding without ordinary quorum under a
special capability with an epoch bump that fences any stale owner.

**The concrete consensus engine sits behind a small internal abstraction.**
Follow-up slices target a narrow seam —
[`replication::control_plane::ControlPlaneConsensus`](../../crates/reddb-server/src/replication/control_plane.rs)
— rather than a specific library or hand-rolled protocol. The seam exposes the
role/term/leader, the committed index, and a leader-only `propose` over a closed
`ControlPlaneEntry` set (membership changes and ownership transitions only). The
entry type is closed by construction so there is *no* user-data variant to
misuse, and the encoding of each entry's payload is owned by the slice that
implements it. Whether the engine is an embedded Raft crate, the in-house
election core extended with a replicated log, or another quorum protocol is an
implementation detail behind this seam; swapping it must not change the boundary.

## Considered Options

- **Raft-equivalent control plane, user data outside it (chosen).** Gives RedDB
  built-in automatic failover and a single auditable order for membership and
  ownership decisions, with the strong safety properties Raft provides, while
  keeping the data path free of consensus latency. Honours ADR 0030's
  decoupling and ADR 0037's transition model.
- **Fully embedded Raft owning the leader *and* the data writes (Mongo/Neo4j
  shape).** Rejected: routing user payloads through a consensus log couples write
  throughput and latency to control-plane quorum and re-entangles the write path
  that ADR 0030 deliberately separated. We adopt Raft's vote/log safety for the
  control plane without paying its cost on every user write.
- **Fully external orchestration only (Patroni/Sentinel).** Rejected as the sole
  model: it leaves RedDB the lone reference system without native failover and
  pushes ownership-catalog authority outside the database. RedDB may still
  *consume* operator-declared desired state, but it keeps native autodetect and
  failover.
- **No consensus — gossip/heartbeat ownership with last-writer-wins.** Rejected:
  without a quorum-committed order, concurrent Supervisors can record divergent
  ownership and two writers can believe they own the same range. The whole point
  of the ownership catalog (ADR 0037) is a single fenced order.
- **Commit a library now (e.g. pin `openraft`).** Deferred, not rejected: the
  seam means a slice can adopt a library later without re-deciding the boundary.
  Choosing one now would over-commit before the log-storage and snapshot slices
  define their needs.

## Safety properties required before implementation

1. **Single leader per term.** A control-plane term has at most one leader.
   Enforced structurally: a win needs a strict majority of voting members, any
   two majorities intersect, and the shared voter grants at most one vote per
   term from durable state.
2. **Committed-entry durability.** A control-plane entry committed (replicated to
   a quorum) is never lost or reordered across any failover or restart. The
   ownership state a committed transition establishes survives leader change.
3. **No double-vote across restart.** A voter that restarts mid-term honours its
   persisted `(term, voted_for)` and refuses a conflicting grant from disk.
4. **Data/control isolation.** No user-data write is recorded in, ordered by, or
   gated on the control-plane log; no control-plane entry carries user-data
   payloads. The closed `ControlPlaneEntry` set enforces the second half at the
   type level.
5. **Fenced ownership transitions.** Ownership transitions written by the leader
   carry term + ownership epoch so a stale former leader/owner cannot apply a
   divergent transition (ADR 0037 fencing, ADR 0032 term framing).

## Consequences

- The election core (issue #834) and witness profile (issue #836) are the
  election half of this layer; a follow-up slice adds the **replicated
  control-plane log** (append/commit + durable log store) behind the same seam.
- The control-plane log needs durable per-node storage distinct from the data
  WAL, plus a snapshot/compaction story for membership + ownership state — a
  named follow-up, not part of this decision.
- Ownership-catalog writes (ADR 0037) route through the leader's `propose` path
  in the normal case; administrative `FORCE` transitions keep their documented
  out-of-band recovery path.
- Implementation slices after #996 depend on the
  [`ControlPlaneConsensus`](../../crates/reddb-server/src/replication/control_plane.rs)
  seam and the boundaries above, and do not re-open the protocol or library
  choice.
