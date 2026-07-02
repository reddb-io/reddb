# ADR 0065 — Transaction Manager v2: complete and harden the live optimistic MVCC engine

**Status:** Accepted (amended 2026-07-02 after code audit)
**Date:** 2026-07-02
**Supersedes:** [ADR 0002](0002-mvcc-promotion.md)
**Related:** PRD [#1620](https://github.com/reddb-io/reddb/issues/1620) (slices #1644–#1651), PRD [#1619](https://github.com/reddb-io/reddb/issues/1619) (phase-1 gate)

## Context

RedDB advertises MVCC and accepts `BEGIN ISOLATION LEVEL …`. ADR 0002
(Draft, 2026-05) described a world with a process-wide
`commit_lock: Mutex<()>` serialising commits and proposed promoting the
dormant `storage::transaction::coordinator::TransactionManager`
(pessimistic lock manager, wait-for-graph deadlock detection, savepoint
stack) into production.

**A 2026-07-02 code audit found that picture stale.** The engine's live
state today:

- **The global commit lock is gone.** No `commit_lock` field exists;
  only two stale comments and a stale transaction-README invariant still
  mention it. Cross-session commit ordering comes from the
  `SnapshotManager`'s xid allocation.
- **Optimistic Snapshot Isolation with first-committer-wins is live in
  production.** `SnapshotManager` + the pure `is_visible` visibility
  predicate + commit-time conflict checks
  (`check_table_row_write_conflicts` et al.) back every SQL
  `BEGIN/COMMIT/ROLLBACK`, with an e2e test proving concurrent updates of
  the same logical row raise a retryable serialization conflict.
- **Savepoints are live** — parsed (`SAVEPOINT` / `RELEASE` /
  `ROLLBACK TO`) and executed via sub-xid allocation in `TxnContext`,
  with e2e coverage.
- The page-level WAL `TransactionManager` (durability) is a separate
  concern with its **own uncoordinated xid space** — a standing
  recovery/debugging hazard.

What is genuinely missing:

1. The parser **discards the parsed isolation level** (`let _ = parts;`)
   and the runtime hardcodes SnapshotIsolation — the keywords have no
   semantics.
2. **`SERIALIZABLE` is rejected by the parser**; SSI does not exist.
3. **READ COMMITTED** has no per-statement-snapshot semantics.
4. The **dormant coordinator** (locks, deadlock detection, a second
   savepoint implementation) still sits in the tree, and the live
   `IsolationLevel` enum is physically homed inside it.
5. **Two uncoordinated XID allocators** (WAL page-TM vs SnapshotManager).
6. **No TLA+ spec** covers MVCC visibility, FCW, or SSI (the existing
   Durability spec models replication watermarks only).
7. **No concurrent multi-writer harness or commit-throughput benchmark**
   exists — commit concurrency is unmeasured.

## Decision

**TM v2 is not a rewrite and not a promotion of the dormant coordinator.
It completes and hardens the live optimistic engine** (SnapshotManager +
visibility predicate + FCW), keeping the concurrency philosophy the
engine already committed to:

### Concurrency model: optimistic, SI default, SSI in the same phase

- Isolation levels become real: the parsed level threads through
  `TxnControl::Begin(IsolationLevel)` into `TxnContext` and is honored.
  READ UNCOMMITTED / READ COMMITTED / REPEATABLE READ / SNAPSHOT map to
  SI initially (legal — levels are minimums), then READ COMMITTED gains
  true per-statement-snapshot semantics.
- **SERIALIZABLE ships via SSI** (PostgreSQL-style rw-antidependency
  tracking with the dangerous-structure abort rule), not via locks.
- **No pessimistic lock manager and no deadlock detection**: purely
  optimistic protocol → no lock waits → deadlock impossible by
  construction. Pessimistic row locking (`SELECT FOR UPDATE`) is a
  possible future extension and would reopen this question.

### Savepoints

Already live (sub-xid design). TM v2 preserves them and extends coverage
(interaction with READ COMMITTED re-snapshotting, DST crash scenarios).

### Dormant coordinator: retired, not deleted

`IsolationLevel` moves to the live transaction module; the coordinator,
its lock manager, its savepoint duplicate, and its transaction log are
gated out of the normal build with retirement notes pointing here. The
stale README/comments describing the removed `commit_lock` are fixed.

### XID unification

One allocation authority (the SnapshotManager); the WAL page-TM draws
from it or from an explicit documented bridge. Recovery
(`observe_committed_xid`, snapshot-xid floor) and the stdio `tx.*`
replay path must remain correct.

### Rollout: direct landing, no coexistence flag

Slices land directly on `main` (owner's call, upheld from the original
decision). With no rollback lever, the guarantee moves ahead of the
merge — **the SSI slice is merge-gated on a new TLA+ spec** of the
FCW/SI/SSI commit protocol checked green in the existing TLC CI lane,
plus a dedicated DST commit-path campaign and a concurrent multi-writer
harness/benchmark that also guards SSI's read-tracking overhead on
SI-only workloads.

### Sequencing

Phase 2 of the roadmap: starts only after all five god-file
decompositions (PRD #1619) complete — the TM surfaces live exactly in
the files being decomposed, and the MVCC pending-action lifecycle
extraction (#1624) hands this phase a dedicated module to extend.

## Consequences

- ADR 0002 stays superseded; its xid-unification insight survives as
  slice #1648, its lock-manager promotion is dead.
- `BEGIN ISOLATION LEVEL` gains real semantics; clients get a documented
  retryable serialization-conflict contract (already partially true for
  SI conflicts today; SSI aborts join the same error class).
- The write-throughput claim ("commits are concurrent") is already true
  mechanically but unmeasured — the Criterion concurrent-commit lane
  makes it a guarded number.
- Anyone reading the tree finds one transaction engine, correctly
  documented, instead of a live engine + a dormant rival + a README
  describing neither.
