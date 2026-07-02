# ADR 0002 — Promote MVCC transaction coordinator into production

**Status:** Superseded
**Date:** 2026-05-04
**Supersedes:** —
**Superseded by:** [ADR 0065](0065-transaction-manager-v2-rewrite.md) — TM v2 rewritten over the #1383 MVCC history store instead of promoting the dormant coordinator
**Related issues:** [#27](https://github.com/reddb-io/reddb/issues/27),
[#28](https://github.com/reddb-io/reddb/issues/28),
[#29](https://github.com/reddb-io/reddb/issues/29),
[#30](https://github.com/reddb-io/reddb/issues/30),
[#31](https://github.com/reddb-io/reddb/issues/31),
[#32](https://github.com/reddb-io/reddb/issues/32)

## Context

RedDB ships two transaction managers today:

1. **`storage::wal::transaction::TransactionManager`** — instantiated
   in `engine::database`, called by every public write entry point
   (SQL DML, gRPC `Insert`/`BulkInsert`, HTTP `POST /collections/X`,
   the native-wire equivalents). Allocates xids from a global
   `AtomicU64` (`NEXT_TX_ID`) and serialises commits through a
   process-wide `RuntimeInner.commit_lock: Mutex<()>`. **No MVCC row
   stamping** beyond `xmin = 0` defaults; **no isolation level
   semantics** beyond the parser accepting the keywords. Durability
   boundary is `wal.sync()` per autocommit sub-statement.

2. **`storage::transaction::coordinator::TransactionManager`** —
   carries `IsolationLevel`, `LockManager` with wait-for graph +
   deadlock detection, savepoint stack, and a separate `AtomicU64`
   for xid allocation. Documented as **DORMANT** in the module
   `README.md`; only its own tests instantiate it. No production
   write path goes through it.

Plus the new visibility deep module from [#27](https://github.com/reddb-io/reddb/issues/27),
`storage::transaction::visibility::is_visible`, which encodes the
full MVCC visibility predicate as a pure function over
`(xmin, xmax, snapshot_xid, in_progress, aborted)`.

The engine surface promises ACID-like semantics in the README and
accepts `BEGIN ISOLATION LEVEL { READ COMMITTED | REPEATABLE READ |
SNAPSHOT | SERIALIZABLE }` in SQL — but the runtime ignores the
isolation level. `e2e_isolation_levels.rs` only verifies the parser
accepts the keywords; no semantic test runs against any of the
levels today.

## Decision

This ADR locks in the path from "two TMs, only one wired, no real
isolation" to "single TM, real snapshot/serializable semantics, no
silent re-routing of existing callers". Five points need committing:

### 1. XID allocator unification

**Decision:** Retire `wal::transaction::NEXT_TX_ID`. The
`coordinator::TransactionManager.next_id` becomes the single
allocator the engine knows about.

**Migration mechanic:** On engine open, read the highest xid stamped
on any persisted row (scan `xmin`/`xmax` over the entity store at
recovery time) and seed the coordinator's allocator at
`max_observed_xid + 1`. The legacy `NEXT_TX_ID` static disappears
in the same commit that wires the coordinator into the write path
(issue #30) — no transition window where both allocators hand out
ids.

**Why a single allocator:** Two `AtomicU64`s issuing xids produce
two non-overlapping namespaces. A row stamped by the legacy
allocator with xid `N` and a row stamped by the coordinator with the
same `N` are indistinguishable on disk and the visibility predicate
silently returns the wrong answer. Coordinated bookkeeping — single
counter, single source of truth.

**Why seed from disk:** Avoids overlap with persisted rows from
earlier engine versions. Pre-MVCC rows carry `xmin = 0` so the
allocator can start at 1 on a fresh database; databases that already
have `xmin > 0` rows on disk seed past the high-water mark.

### 2. Row-header format bump policy

**Decision:** Use the existing `xmin`/`xmax` fields on
`UnifiedEntity` (already in the struct, populated as `0/0` for pre-
MVCC rows). No format bump — the bytes are already on disk; only
the values change going forward.

**Migration mechanic:** Lazy-stamp on first MVCC-touching operation.
A pre-MVCC row (`xmin == 0`) stays `xmin == 0` until something
updates or deletes it. The visibility predicate already treats
`xmin == 0` as "visible to every snapshot" (rule 1 in
`visibility.rs`), so existing rows are correct without any one-shot
migration.

**Why lazy migration:** Eager-stamp would rewrite every page of every
collection on first open after upgrade — minutes-to-hours of stall on
a TB-scale database. Lazy keeps RTO unchanged. The cost is one extra
match arm in the visibility predicate (already there: rule 1) and
the loss of the ability to attribute a pre-MVCC row to any specific
historical transaction (acceptable; those rows predate MVCC's
existence).

**Why no format bump:** Adding bytes to the on-disk record forces a
page rewrite on first read; we'd be paying the eager-migration cost
through the back door. Reusing the existing fields keeps the upgrade
in-place and bytewise-compatible with v0.x snapshots.

### 3. Performance budget

**Decision:** SNAPSHOT isolation must not regress the existing
single-mutex commit path by more than the budget below. SERIALIZABLE
is allowed to be slower; reads under SNAPSHOT must not regress.

| Bench (pre-promotion baseline)              | Pre  | Budget under SNAPSHOT | Budget under SERIALIZABLE |
|---------------------------------------------|------|------------------------|----------------------------|
| `bench_insert::single_row_autocommit`       | 1.0× | ≤ 1.10× (10% slower)   | ≤ 1.50× (50% slower)       |
| `bench_insert::bulk_insert_25k`             | 1.0× | ≤ 1.05× (5% slower)    | ≤ 1.30× (30% slower)       |
| `bench_embedded::query_select_point`        | 1.0× | ≤ 1.05× (5% slower)    | ≤ 1.05× (no extra cost)    |
| `bench_embedded::query_select_range`        | 1.0× | ≤ 1.05× (5% slower)    | ≤ 1.10× (10% slower)       |
| Concurrent reader/writer mix (perf_sweep)   | 1.0× | ≥ 1.15× faster (no global lock) | ≤ 1.10× slower    |

The "concurrent mix" line goes the right way under SNAPSHOT because
the existing `commit_lock: Mutex<()>` serialises all commits;
removing it should *help* concurrent throughput once MVCC reads
stop blocking on the writer's mutex.

**Numbers are ratios, not absolutes.** Re-run the bench suite on the
target hardware before each MVCC slice merges; record before/after
in the slice's PR description.

**Rollback plan:** Each slice (#29, #30, #31, #32) lands behind a
feature gate `mvcc_promotion = "off" | "snapshot" | "serializable"`
read from `red.config.mvcc.mode` (KV-backed, hot-reloadable). Set
to `"off"` to fall back to the legacy single-mutex path. The gate
disappears one minor release after #32 ships and the benches stay
within budget; until then, operators have an opt-out.

### 4. Staging order

**Decision:** Reads first, writes second, multi-statement third,
serializable last. Specifically:

1. **#29 — Snapshot-isolated single-statement reads.** Allocates a
   per-statement snapshot via `SnapshotManager::snapshot()`,
   delegates the visibility check to `is_visible`. Writes still go
   through the legacy `wal::transaction.rs` allocator. Rows stamped
   with `xmin = 0` (every existing row) stay visible — the new code
   path is invisible to every existing test.
2. **#30 — Single-statement writes through the coordinator.** Now
   every INSERT/UPDATE/DELETE stamps `xmin` from the unified
   allocator. Reads from #29 see the new stamps via `is_visible`.
   The legacy `NEXT_TX_ID` static is removed in this slice.
3. **#31 — Multi-statement BEGIN/COMMIT under SNAPSHOT.** Replaces
   `commit_lock: Mutex<()>`. The runtime carries a per-connection
   `TxnContext` (already declared in `snapshot.rs`); BEGIN allocates,
   COMMIT publishes, ROLLBACK marks aborted.
4. **#32 — SERIALIZABLE + lock manager.** Promotes
   `transaction::lock::LockManager` from dormant to live for
   SERIALIZABLE transactions. Phantom-read + deadlock tests added.

**Why reads first:** The read path is read-only; landing it doesn't
change any persisted state. If it regresses, the rollback is "set
the gate to off" with zero data risk. Landing writes first and reads
second would create a window where rows have new `xmin` stamps but
no reader consults them — silent correctness gap.

**Why serializable last:** SERIALIZABLE needs the lock manager,
which needs the multi-statement transaction context, which needs
the writer-side xid stamping, which needs the read-side snapshot.
Each prior slice is the foundation the next one builds on; doing
them out of order forces parallel half-baked branches.

### 5. Scope of `BEGIN ISOLATION LEVEL` for v1.0

**Decision:** v1.0 ships `READ COMMITTED` (default) and `SNAPSHOT`.
`REPEATABLE READ` is a synonym for `SNAPSHOT` (PostgreSQL parity —
PG's `REPEATABLE READ` is snapshot isolation). `SERIALIZABLE` ships
in v1.0 if #32 lands within the perf budget; otherwise it stays
parser-accepted with a runtime downgrade-to-SNAPSHOT + `tracing` warn
(matches the existing comment in `snapshot.rs`).

**The four levels:**

| Level             | v1.0 status      | Semantics                          |
|-------------------|------------------|-------------------------------------|
| READ UNCOMMITTED  | rejected at parse | Operator must use READ COMMITTED   |
| READ COMMITTED    | shipped, default | Per-statement snapshot              |
| REPEATABLE READ   | shipped (synonym for SNAPSHOT) | One snapshot per transaction |
| SNAPSHOT          | shipped          | One snapshot per transaction        |
| SERIALIZABLE      | conditional on #32 perf | SSI; falls back to SNAPSHOT on regression |

**Why drop READ UNCOMMITTED:** It's unsafe under MVCC (would require
exposing in-progress writers) and no caller asks for it today. Reject
at parse with a clear message naming the alternatives.

## Consequences

### Positive

- **Real ACID** for the first time. Today's runtime concedes through
  documentation; v1.0 shipping with #32 closes the gap.
- **Single allocator** removes a class of correctness bugs that nobody
  has hit yet because both allocators have always been single-process.
- **Deletes the global commit mutex** — concurrent writes against
  different keys stop serialising on each other under SNAPSHOT.
- **Tests in `e2e_isolation_levels.rs` become real semantic tests**
  rather than parser smoke.

### Negative

- **Slower autocommit writes.** Even in the best case (SNAPSHOT
  rejecting nothing, no contention), the per-write xmin stamp costs
  a few cycles. Budget is 5–10% on the hot path; the rollback gate
  is the safety net if we miss.
- **More state on every entity.** The `xmin`/`xmax` fields are
  already there; what's new is that they're meaningful values now,
  which means VACUUM has work to do (reclaim aborted xids,
  truncate the version chain). Vacuum exists in tree but is not
  scheduled by default; this ADR does not turn it on. A follow-up
  issue covers the operator-facing vacuum surface.
- **One feature gate to maintain** for one minor release. Acceptable.

### Neutral

- **Recovery still uses WAL fsync as the durability boundary.**
  No change to the disk-loss / process-crash story. RTO/RPO numbers
  in `docs/operations/rto-rpo.md` stay correct.
- **Replication on disk format unchanged.** Replicas continue to
  receive the same logical change records they receive today;
  `xmin`/`xmax` round-trip in the existing format.

## Alternatives considered

1. **Keep both TMs and gate per-call.** Rejected — every public
   write path would need to know which TM to ask for the xid. Two
   sources of truth, double the surface to test, no upside.
2. **Hard cutover with a one-shot CLI tool.** Rejected — pre-MVCC
   rows already work under the new visibility predicate (`xmin == 0`
   is rule 1: always visible). Eager migration would burn hours of
   stall for zero correctness gain.
3. **Predicate locking over snapshot isolation.** Rejected for
   v1.0 — PG-style SSI (Serializable Snapshot Isolation) is the path,
   not 2PL. SSI builds on SNAPSHOT plus a wait-for-conflict graph;
   the lock manager already in `lock.rs` is the wait-for piece.
4. **Skip SERIALIZABLE entirely for v1.0.** Considered — the rolling
   downgrade-to-SNAPSHOT exists today. Decision is to ship if the
   perf budget holds, downgrade if it doesn't. Either way the parser
   keyword stays accepted so user code doesn't break.

## Open questions

- **Do we need `XidEpoch` (32-bit xid + 32-bit epoch) at v1.0?** PG
  rolls every ~2 billion xids; RedDB's allocator is u64, so wraparound
  is ~290B-billion years out at sustained 1M tx/s. No epoch field
  proposed for v1.0. Revisit if anyone runs a single instance >100
  years.
- **Vacuum scheduling default.** Off in this ADR. The follow-up
  `vacuum-scheduling` issue picks the default cadence.
- **Replica xid alignment.** Today replicas mint their own xids when
  applying logical changes; under MVCC promotion, replicas must
  preserve the primary's xid so cross-replica reads see the same
  visibility. Tracked in a separate replication-xid issue.

---

**Reviewers:** This is the gating decision for issues #29-#32.
Approving this ADR unblocks the implementation chain. Holding the
ADR open holds the chain — the autonomous loop will not progress
through #29 until this lands.
