# TLA+ specs for the replication consensus core

Formal models of RedDB's term-based, quorum-gated replication safety
(issue #841, PRD #819). Each module asserts **one** safety property and is
model-checked by TLC in CI (`.github/workflows/tla-check.yml`). The models are
deliberately small (3 nodes / 3 terms) so they exhaust their state space in
seconds-to-minutes, not hours.

The randomized executable counterpart is the
[replication Maelstrom-style protocol model](../../docs/testing/replication-maelstrom-model.md).
It replays the same safety envelope under deterministic message loss, delay,
reorder, partition, crash, and restart schedules; it is not the later black-box
Jepsen-style cluster harness.

| Module | Property proved | Maps to |
| --- | --- | --- |
| [`ElectionSafety.tla`](ElectionSafety.tla) | No two nodes hold primary in the same term. | `replication/election.rs` (`Voter::consider`, `quorum_threshold`); ADR 0030 "no two primaries in a term" (#834). |
| [`Durability.tla`](Durability.tla) | A write at/below the commit watermark is never rolled back (`CommittedNeverRolledBack` + `LeaderCompleteness`). | ADR 0030 commit-watermark / `NeverRollbackCommitted`; ADR 0032 `(term, lsn)` framing; `replication/quorum.rs`, `commit_waiter.rs`, `rollback.rs`. |
| [`SafeReconfig.tla`](SafeReconfig.tla) | Every membership change preserves quorum overlap (old and new quorums intersect). | ADR 0030 membership rules; Raft single-server change. |

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

## Running locally

Needs a JDK (17+) and `tla2tools.jar` (pinned to `v1.8.0` in CI):

```sh
curl -fsSL -o tla2tools.jar \
  https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar

# Check one module (repeat for Durability, SafeReconfig):
java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC \
  -deadlock -config ElectionSafety.cfg ElectionSafety.tla
```

`-deadlock` disables deadlock checking on purpose: these are bounded models, so
terminal states (no further enabled action) are expected and are not errors.
TLC exits non-zero on a violated invariant, which fails the CI job.
