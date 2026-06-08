# Replication Consistency and Failover Model

Status: proposed

RedDB today does single-primary, multi-replica replication with async-by-default
commit (`local` policy), region-aware quorum (`QuorumMode::Regions`), a CAS writer
lease for serverless single-writer fencing, and **manual** failover — there is no
election, no consensus, and no term/epoch on the WAL. This ADR fixes the target
consistency and failover model that the replication roadmap builds on. It governs
*who decides the leader*, *what a write acknowledgement guarantees across a
failover*, and *what happens to a deposed primary*. It does not cover the wire
transport, the causal-read token, retention slots, or the replication-stream
authorization model — those are separate decisions that depend on this one.

The driving goal is to be strictly better than the reference systems studied
(PostgreSQL, MongoDB, Neo4j, Valkey): keep PostgreSQL's discipline of not
entangling election with the write path, while closing PostgreSQL's one real gap
(no built-in automatic failover) the way Mongo/Neo4j/Valkey do.

## Decisions

**Control plane shape — first-party but decoupled.** Election and automatic
promotion are built into RedDB (so RedDB is not the only system without automatic
failover), but live in a *separable control-plane supervisor module* distinct from
the data plane, not entangled in the write path. The supervisor reuses the
existing ack-tracking infrastructure (`CommitWaiter` replica ack state) and the
writer lease's `generation` counter rather than introducing a parallel state
machine. This honours PostgreSQL's lesson ("keep the engine composable; don't bury
a hidden failover state machine in the write path") while delivering the automatic
failover that Mongo (embedded Raft) and Valkey (external Sentinel) provide.

**Election quorum — data members vote, plus vote-only witnesses.** The nodes that
hold data are the voters; a quorum is a majority of voting members (Mongo/Neo4j
model), avoiding a separate supervisor fleet just to obtain HA. A lightweight
**Witness** member (vote-only, holds no data, runs only the supervisor module) is
supported so that `2 data nodes + 1 witness` is a valid HA shape (Mongo arbiter
idea). The supervisor is therefore a module every node runs; a witness is a node
that runs *only* that module.

**Durability guarantee — strong for synchronous writes.** A write acknowledged
under a synchronous commit policy (`ack_n` / `quorum`) is **guaranteed** to survive
any failover. This is enforced by the election vote rule: a candidate may only win
if its log covers the **Commit watermark** — the highest LSN durably replicated to
a quorum that intersects every possible election majority. Nothing at or below the
watermark is ever rolled back (Mongo's `NeverRollbackCommitted`). Writes under the
fast `local` policy carry no such guarantee, and that is documented, not hidden.
The commit watermark is the single stability anchor for synchronous-read
visibility, the rollback bound, and (later) the causal-read token.

**Default commit policy — adaptive by declared role, never silent degrade.** A
standalone deployment (no replicas declared) defaults to `local`. A deployment
that declares replicas (HA intent) defaults to `quorum`, and when a quorum cannot
form it **refuses the write** (`min-replicas-to-write` self-fence) rather than
silently degrading to `local`. This rejects both footguns: stalling a single-node
embedded/serverless deployment, and silently dropping the durability guarantee at
the exact moment a replica is down (the criticism of Mongo's implicit-default
write-concern adaptivity). Refusing is honest; degrading quietly is not.

**Deposed-primary reconciliation — auto-rollback with tail preservation.** A
former primary that rejoins after a failover holding writes below the commit
watermark (a divergent tail) automatically recovers-to-LSN at the common point and
rejoins as a replica, using the MVCC history store (ADR 0014) for the
recover-to-LSN — so failover is self-healing and does not page an operator every
time. The discarded tail is, by definition, non-committed (below the watermark), so
removing it from the live timeline is correct; but it is **always** persisted to
rollback files and surfaced via a loud `OperatorEvent` so the lost writes remain
auditable and reconcilable. Rollback is never silent.

## Considered Options

- **Fully external orchestration (PostgreSQL/Patroni, Valkey/Sentinel).** Rejected
  as the *only* model: leaves RedDB the lone reference system without built-in
  automatic failover. Its discipline is preserved by decoupling the supervisor,
  not by exporting it entirely.
- **Fully embedded Raft owning the leader (Mongo/Neo4j).** Rejected as the shape:
  it tends to entangle consensus with the write path. We take the term/epoch and
  vote-safety ideas without running data payloads through a Raft log.
- **Best-effort failover (pick the most-caught-up replica, no hard guarantee).**
  Rejected: that is precisely what lets Valkey lose acknowledged writes; it is
  incompatible with the strong synchronous-durability guarantee above.
- **Fast-everywhere or safe-everywhere default.** Rejected in favour of the
  role-adaptive default, which avoids the single-node stall and the silent-unsafe
  degrade simultaneously.

## Consequences

- A **term/epoch must be stamped into the WAL/logical-spool framing** (a new
  framing version) — divergence detection and the vote rule are ambiguous without
  it. This is the first roadmap work item that this ADR forces.
- The `QuorumCoordinator` (currently poll-based) and `CommitWaiter` (condvar-based)
  must converge on one ack path that also feeds the election vote rule.
- The supervisor needs durable per-node vote state (last-vote) to prevent
  double-voting across restarts.
- Witness members require a build/runtime profile that excludes the data plane.
- Auto-rollback depends on MVCC history (ADR 0014) being retained at least back to
  the common point; retention policy must account for this.
