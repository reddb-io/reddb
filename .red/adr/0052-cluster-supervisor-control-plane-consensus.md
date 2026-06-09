# Cluster Supervisor Control-plane Consensus

Status: accepted

RedDB's multi-writer cluster (PRD #987) needs one safe authority for global
control-plane state — membership, Supervisor leadership, and the shard/range
ownership catalog — while keeping user-data writes fast and horizontally
scalable. This ADR fixes *the concrete control-plane consensus approach* so that
follow-up implementation slices can proceed without re-choosing a protocol or a
library. It is the resolution of the HITL decision recorded on issue #996
(2026-06-06) and the commitment behind ADR 0030's "first-party but decoupled"
supervisor and ADR 0037's "the Cluster Supervisor must update ownership through
fenced, versioned transitions".

It does **not** specify the wire encoding of the control-plane log, the on-disk
format of the durable vote/log state, the full ownership-catalog schema, or the
rebalancer policy — those are separate follow-up slices that build on the
boundary fixed here.

## Decision

**Use a Raft-equivalent control-plane consensus layer for the Cluster
Supervisor — and only for the Cluster Supervisor.** The layer carries
Supervisor membership, leader election, durable vote/log state, and global
shard/range ownership-catalog transitions. It does not carry user data.

The decision has five concrete parts.

**1. Raft-equivalent, not a chosen Raft library.** The control plane gets a
replicated log with Raft's safety semantics — monotonic election terms, a
durable per-node last-vote, majority quorum, a single leader per term, and a
totally-ordered committed log. RedDB already implements exactly these vote-safety
mechanics for primary election (`crate::replication::election`, ADR 0030: term
bump, durable last-vote, watermark vote rule, "no two leaders in a term"). The
Supervisor reuses those mechanics rather than importing a third-party Raft crate
in this slice. "Equivalent" is the operative word: a later slice may back the log
with an external engine, but only behind the abstraction below — callers never
see the choice.

**2. User-data writes stay outside the control-plane log — structurally.** The
control-plane log's entry type (`ControlPlaneEntry`) has *no* variant capable of
carrying a row, document, queue message, or any user payload. A user write is
therefore unrepresentable in the control-plane log by construction, not merely by
convention. This is the central boundary: durable user writes are never routed
through, gated by, or made to wait on Supervisor consensus, so control-plane
availability cost is not paid on the user-data hot path (PRD #987 user stories
11, 24; ADR 0030 "without running data payloads through a Raft log"). User-data
durability remains the per-range commit policy and the WAL/logical replication
stream (ADR 0030, ADR 0044), which are unchanged by this decision.

**3. Durable vote/log state is a required part of the boundary.** Supervisor
leader election requires each voting member to persist its `(term, voted_for)`
*before* acknowledging a vote, so a member that crashes and restarts mid-term
cannot double-vote and elect two leaders for one term. The committed
control-plane log entries are likewise durable and replicated to the voting
members, so a new leader recovers the agreed membership and ownership state
rather than reconstructing it from local guesses. Both durability requirements
are named in the abstraction (`DurableVoteState`, the committed log) so a
follow-up slice cannot ship election without them.

**4. The Supervisor leader is the normal writer for ownership-catalog
transitions.** Ownership transitions (move, split, merge, promote, drain — ADR
0037) are appended to the control-plane log by the elected Supervisor leader and
only by it. A follower that wants a transition forwards the request to the
leader; it does not write the catalog locally. Each transition carries the
ownership epoch that fences a stale old owner (ADR 0037 "Fencing is enforced
below routing"), and recording it in the consensus log makes that fence durable
and globally agreed. Forced disaster-recovery transitions remain the documented
exception (ADR 0037 "Forced transitions are reserved for disaster recovery") —
they still bump the ownership epoch and leave durable audit evidence, but may
proceed without ordinary quorum under a special administrative capability.

**5. The concrete engine sits behind a small internal abstraction.** A single
trait, `cluster::control_plane::ControlPlaneConsensus`, is the seam every
follow-up slice depends on: read the current term and elected leader, append a
`ControlPlaneEntry` (leader-only), and read the durable vote and commit index.
The first implementation is a degenerate single-node engine
(`SingleNodeControlPlane`: this node is the sole voter and the leader of term 1);
a later slice can swap in a fully replicated, persisted log without touching
callers. Leader-only append is enforced by the trait (a follower's append
returns `NotLeader`), so part 4 is mechanism, not documentation.

## Considered Options

- **Embed a third-party Raft crate now (e.g. `openraft`, `raft-rs`) owning the
  Supervisor log.** Rejected *for this slice*, not forever. RedDB already has the
  vote-safety primitives (term/durable-last-vote/quorum) from primary election;
  pulling in a second consensus stack now would duplicate that machinery and
  couple the first cut to a specific crate's storage/transport traits before the
  control-plane log's shape is settled. The abstraction keeps this option open
  behind the trait.

- **Run user-data writes through the same control-plane Raft log (Mongo/Neo4j
  "embedded Raft owns everything" shape).** Rejected — it is the exact coupling
  PRD #987 and ADR 0030 set out to avoid. It would force every durable user write
  to pay control-plane consensus latency and tie user-write availability to
  Supervisor quorum, defeating the multi-writer scaling goal. The closed entry
  type makes this mistake unrepresentable.

- **No formal consensus — gossip/CRDT or "most-caught-up node wins" for the
  ownership catalog.** Rejected: ownership is single-writer-per-range authority;
  two nodes briefly believing they own the same range is a correctness violation,
  not eventual-consistency noise. The catalog needs a single leader per term and a
  totally-ordered log, which is precisely what a Raft-equivalent layer provides
  and a leaderless model does not.

- **Fully external orchestration (PostgreSQL/Patroni, Valkey/Sentinel) for
  Supervisor election.** Rejected as the *only* model for the same reason as ADR
  0030: it leaves RedDB the lone reference system without built-in automatic
  control-plane failover. Operators may still *declare desired state* from
  external infrastructure (PRD #987 user story 10), but RedDB remains capable of
  native election and ownership decisions.

- **Whole-collection ownership recorded as plain catalog rows edited directly.**
  Rejected (consistent with ADR 0037): ownership changes are fenced, versioned,
  audited *transitions* written by the leader, not arbitrary row edits, so
  recovery and automation share one safe path.

## Safety properties required before implementation slices proceed

These are the invariants a follow-up slice must preserve; they are the
acceptance contract for everything built on this boundary.

- **One leader per term.** A win requires a strict majority of voting members,
  and a member votes at most once per term (durable last-vote), so two majorities
  cannot elect two leaders for one term even under an arbitrary partition. (Same
  structural argument as primary election, ADR 0030.)
- **No user data in the control-plane log.** The entry type cannot encode a user
  write. Adding such a variant is a decision reversal requiring a new ADR.
- **Durable vote before acknowledgement.** `(term, voted_for)` is persisted
  before a vote is acknowledged; a restart never causes a double vote.
- **Leader-only catalog writes.** Ownership-catalog transitions are appended only
  by the current Supervisor leader; followers forward, they do not write.
- **Epoch-fenced transitions.** Every ownership transition bumps the ownership
  epoch so a stale owner that reappears is fenced below routing (ADR 0037), and
  the epoch is recorded durably in the consensus log.
- **Forced transitions are the audited exception.** A `FORCE` transition may skip
  ordinary quorum only with a special capability, an explicit operator reason,
  durable audit evidence, and an ownership-epoch bump.

## Consequences

- Follow-up slices depend on the `ControlPlaneConsensus` trait and the
  `ControlPlaneEntry` type, not on a consensus library or an invented protocol;
  the protocol choice is closed for now and reopening it requires a new ADR.
- The first cut ships `SingleNodeControlPlane` (a one-voter majority). Multi-node
  Supervisor election and a replicated, persisted control-plane log are explicit
  later slices that implement the same trait — the durable on-disk format of the
  vote/log state is one of them and is intentionally not fixed here.
- The ownership-catalog schema (ADR 0037 / ADR 0045) is carried *as* control-plane
  log entries; the catalog slice defines the row shape, this ADR fixes only that
  transitions flow through the leader's log with an ownership epoch.
- User-data replication (ADR 0030 commit policy, ADR 0044 logical stream) is
  untouched: this ADR adds a parallel, separate control plane, it does not move
  user writes onto it.
- ADR 0030's primary-election term/last-vote machinery is reused by the Supervisor
  rather than duplicated; if that machinery changes, the control plane inherits
  the change.
