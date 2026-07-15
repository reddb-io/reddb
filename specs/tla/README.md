# TLA+ specs for RedDB safety models

Formal models of RedDB's replication and transaction safety contracts. Each
module asserts a focused bounded safety envelope and is model-checked by TLC in
CI (`.github/workflows/tla-check.yml`). The models are deliberately small so
they exhaust their state space in seconds-to-minutes, not hours.

The randomized executable counterpart is the
[replication Maelstrom-style protocol model](../../docs/testing/replication-maelstrom-model.md).
It replays the same safety envelope under deterministic message loss, delay,
reorder, partition, crash, and restart schedules; it is not the later black-box
Jepsen-style cluster harness.

| Module | Property proved | Maps to |
| --- | --- | --- |
| [`ElectionSafety.tla`](ElectionSafety.tla) | No two nodes hold primary in the same term. | `replication/election.rs` (`Voter::consider`, `quorum_threshold`); ADR 0030 "no two primaries in a term" (#834). |
| [`Durability.tla`](Durability.tla) | A write at/below the commit watermark is never rolled back (`CommittedNeverRolledBack` + `LeaderCompleteness`). | ADR 0030 commit-watermark / `NeverRollbackCommitted`; ADR 0032 `(term, lsn)` framing; `replication/quorum.rs`, `commit_waiter.rs`, `rollback.rs`. |
| [`ReplicationSafetyEnvelope.tla`](ReplicationSafetyEnvelope.tla) | Single-writer fencing, election safety, partition behavior, and crash/recovery durability in one bounded model. | ADR 0030 / ADR 0032; writer fencing, quorum commit, crash/restart recovery. |
| [`SafeReconfig.tla`](SafeReconfig.tla) | Every membership change preserves quorum overlap (old and new quorums intersect). | ADR 0030 membership rules; Raft single-server change. |
| [`CommitProtocol.tla`](CommitProtocol.tla) | Optimistic MVCC commit safety: no lost update under FCW, stable snapshot reads, and SSI-admitted histories are serializable. | ADR 0065 TM v2; `SnapshotManager::begin` / `snapshot` / `commit`; `visibility::is_visible`; SQL table-row commit conflict checks; `TxnContext` savepoint sub-xids. |
| [`OwnershipTransition.tla`](OwnershipTransition.tla) | Range ownership transitions preserve one accepting owner per range epoch and never lose writes acknowledged at/below the commit watermark. | ADR 0037 shard/range ownership catalog; ADR 0064 Placement Authority and Range Owner; ownership admission gate (#1836); ownership-transition safety (#1838). |

## How the models mirror the implementation

* **Election** — a voter's durable last-vote is `(term, voted_for)` exactly as
  the `LastVote` struct; the model encodes the same decision order
  (stale-term refusal, one grant per term), and a win needs a strict majority
  of voting members. Two majorities of one set always intersect and the shared
  voter votes once per term, so at most one primary per term — TLC confirms it
  structurally.
* **Durability** — logs are sequences of terms, entries identified by
  `(index, term)` (ADR 0032). The election vote rule is modelled by Raft's
  "at least as up-to-date" check, the abstraction of `Voter::consider`'s
  watermark clause (`RefusalReason::WatermarkNotCovered`): a quorum holding a
  committed write refuses any candidate that lacks it. `AppendEntries` is
  term-guarded (a stale primary cannot push onto a newer-term node — the
  stand-in for the #835 stale-term fence) and truncates only on a real conflict
  (Raft §5.3). Commit follows Raft's current-term rule, which is what avoids the
  Figure-8 hazard. Leader Completeness is keyed on the **commit term**: a
  deposed primary still flagged `primary` at an old term is excused — it is
  exactly the divergent-tail case ADR 0030 heals by auto-rollback on rejoin.
* **Reconfiguration** — membership changes one voting member at a time (the
  safe Raft rule), and TLC checks that a quorum of the pre-change config always
  intersects a quorum of the post-change config. `JumpBreaksOverlap` documents
  why a multi-member jump is unsafe, so the property is not vacuous.
* **Commit protocol** — `Begin` abstracts `SnapshotManager::begin()` plus
  `snapshot()`: xids are allocated monotonically, and the begin snapshot records
  the active in-progress set. `VisibleWriter` mirrors
  `storage::transaction::visibility::is_visible`: writers with future xids or
  xids active in the reader's snapshot are hidden. `Commit` performs the live
  first-committer-wins check over logical rows before admitting writes. The
  savepoint actions model `TxnContext` sub-xid frames: `SAVEPOINT` allocates a
  frame, `ROLLBACK TO` removes that frame's writes, and `RELEASE` keeps them for
  parent commit. For SSI, `NewRwEdges` records rw-antidependencies and
  `AbortSSI` rejects a commit that would create a dangerous structure. The
  serializability invariant compares every all-SSI admitted history against a
  serial-execution oracle on the same bounded rows.
* **Ownership transition** — the shard/range ownership catalog is the authority
  for `(range, owner, epoch)`, while the local lease/admission gate must match
  that catalog epoch before a durable write is accepted. Normal promotion creates
  the first owner, cooperative handoff and crash failover promote only candidates
  that cover the commit watermark, and forced transitions use the same epoch bump
  and watermark coverage rule while recording audit evidence. Recovery may
  truncate divergent suffixes but cannot discard an acknowledged entry already
  present on that node. The checked invariants assert that only one node ever
  accepts durable writes for a range epoch and that the current owner always
  retains every write acknowledged at or below the range commit watermark.

## Commit protocol non-vacuity

`CommitProtocol.tla` names the witness predicates checked while developing the
bounded model:

* `FCWConflictReached` — the state space contains a commit-time FCW abort for
  overlapping transactions that write the same logical row.
* `DangerousStructureReached` — the state space contains an SSI abort caused by
  two consecutive rw-antidependency edges.
* `OptimisticAdmitsMoreThan2PL` — the state space contains an all-SSI history
  with a read/write overlap admitted by the optimistic protocol, documenting
  that the model is not equivalent to naive strict two-phase locking.

The CI config keeps all transactions on the SSI path (`EnabledLevels = {"SSI"}`)
because SSI includes the Snapshot Isolation visibility and FCW checks while
making the serial-oracle property non-vacuous in a small three-transaction,
two-row model. For local reachability checks, temporarily add
`NoFCWConflictReached`, `NoDangerousStructureReached`, or
`NoOptimisticAdmitsMoreThan2PL` as an invariant; TLC must reject the model with
a counterexample reaching the corresponding witness state. These negated
witness invariants are not in CI because their success condition is failure.

## Running locally

Needs a JDK (17+) and `tla2tools.jar` (pinned to `v1.8.0` in CI):

```sh
curl -fsSL -o tla2tools.jar \
  https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar

# Check one module (repeat for Durability, SafeReconfig, CommitProtocol,
# OwnershipTransition):
java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC \
  -deadlock -config ElectionSafety.cfg ElectionSafety.tla
```

`-deadlock` disables deadlock checking on purpose: these are bounded models, so
terminal states (no further enabled action) are expected and are not errors.
TLC exits non-zero on a violated invariant, which fails the CI job.
