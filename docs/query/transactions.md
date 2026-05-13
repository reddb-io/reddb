# Transactions & MVCC

RedDB implements PostgreSQL-style transaction control with snapshot
isolation. Every connection can open a transaction, nest savepoints,
and roll back to any level without affecting other connections.

The v1 table-row guarantee is the MVCC history-store contract from
[ADR 0014](../adr/0014-mvcc-history-store-and-transaction-recovery.md)
and [PRD #432](https://github.com/reddb-io/reddb/issues/432): SQL
table rows use stable logical identity, versioned `UPDATE`, tombstone
`DELETE`, first-committer-wins conflict checks, atomic `TxCommitBatch`
recovery, and manual `VACUUM` for obsolete history.

Non-table models keep their existing documented transaction behavior
until each path adopts the same history-store resolver. Some paths
already route reads through the shared `xmin` / `xmax` visibility
resolver, so they participate in single-node transaction visibility,
but RedDB does not claim versioned `UPDATE`, historical index fallback,
or old-snapshot history-store reads for every model yet.

```sql
BEGIN;
INSERT INTO orders (id, total) VALUES (1, 100);
INSERT INTO social NODE (label, name) VALUES ('User', 'alice');
INSERT VECTOR INTO embeddings (id, dense) VALUES (1, [...]);
QUEUE PUSH fulfillment {order_id: 1};
COMMIT;
-- table-row MVCC uses the history-store contract; non-table model
-- behavior is limited to the documented path for that model
```

## Quick reference

| Statement | Effect |
|-----------|--------|
| `BEGIN` / `START TRANSACTION` | Open a transaction; allocate `xid` |
| `COMMIT` | Publish the transaction's staged writes |
| `ROLLBACK` | Discard staged writes |
| `SAVEPOINT name` | Push a sub-transaction level |
| `RELEASE SAVEPOINT name` | Pop savepoint (work survives) |
| `ROLLBACK TO SAVEPOINT name` | Abort sub-xids above the savepoint |
| `VACUUM [table]` | Reclaim obsolete table-row history and tombstones not pinned by active snapshots |

## Basic usage

```sql
BEGIN;
INSERT INTO users (id, email) VALUES (1, 'a@b');
UPDATE users SET email = 'x@y' WHERE id = 1;
COMMIT;
```

If the application crashes or the connection drops before `COMMIT`,
in-flight transaction state is discarded because the WAL never sees a
complete commit record.

## Isolation

Each `BEGIN` captures a snapshot — a frozen view of the database that
excludes every transaction still in flight at the moment of capture.
All reads inside the transaction see that snapshot:

```sql
-- session A
BEGIN;
SELECT count(*) FROM users;   -- 100

-- session B  (concurrent)
INSERT INTO users ...;         -- adds row
COMMIT;

-- back in session A
SELECT count(*) FROM users;   -- still 100 — B's row is invisible
COMMIT;

SELECT count(*) FROM users;   -- 101 — autocommit sees latest
```

Own writes are always visible inside the writing transaction even when
the sub-xid exceeds the captured snapshot (needed so the writer can
observe its own savepoint work).

RedDB uses snapshot isolation with first-committer-wins conflict
detection for table rows. If two concurrent transactions update or
delete the same logical row, the first transaction to commit wins; the
later conflicting commit fails with a serialization conflict instead
of silently overwriting the committed version.

## Savepoints

`SAVEPOINT` opens a nested sub-transaction. Writes inside the
savepoint are stamped with a dedicated sub-xid so they can be rolled
back independently of the parent.

```sql
BEGIN;
INSERT INTO users (id, email) VALUES (1, 'a@b');

SAVEPOINT try_risky;
UPDATE users SET email = NULL WHERE id = 1;
-- oops, violates downstream invariant
ROLLBACK TO SAVEPOINT try_risky;
-- row 1 has email = 'a@b' again

INSERT INTO users (id, email) VALUES (2, 'c@d');
COMMIT;
-- final state: two rows, email columns untouched by the bad UPDATE
```

`RELEASE SAVEPOINT name` pops the savepoint but promotes its writes
into the enclosing scope — equivalent to "the sub-work succeeded, merge
it up".

## MVCC tombstones

For SQL table rows, `DELETE` writes a tombstone version for the row's
logical identity instead of physically removing the row immediately:

1. The pre-delete row version is retained in the history store.
2. The current store records a tombstone for the same `logical_id`.
3. Other active snapshots can still resolve the historical version.
4. `VACUUM` later reclaims the tombstone and historical version only
   after no active transaction, snapshot, VCS pin, or retention horizon
   can still need them.

Queue `ACK` (via `QUEUE POP`) inside a transaction keeps its existing
tombstone-based transaction behavior: the message is not made
permanently unavailable until the transaction commits. That is not the
same as the table-row history-store guarantee.

```sql
BEGIN;
QUEUE POP jobs;               -- tombstones the message for this session
-- business logic fails
ROLLBACK;
-- message is visible again for the next consumer
```

Autocommit table-row `DELETE` uses the same tombstone/history path as
explicit transactions. Physical removal is a maintenance concern for
manual `VACUUM`.

## Commit recovery boundary

Committed table-row transactions are durable at the `TxCommitBatch`
boundary. Recovery applies only complete, valid commit batches; an
incomplete batch or in-flight transaction is discarded after restart.
Complete committed batches are replayed idempotently.

The commit path preserves WAL-before-data ordering: RedDB validates
conflicts, builds and appends the `TxCommitBatch`, makes the batch
durable according to the configured WAL policy, applies current-store,
history-store, and index changes, then acknowledges success.

## Versioned UPDATE

For SQL table rows, `UPDATE` creates a new physical version for the
same logical row. The previous committed version is retained as
history and remains visible to snapshots that started before the
update committed.

```sql
-- session A
BEGIN;
SELECT status FROM orders WHERE id = 1; -- 'pending'

-- session B
UPDATE orders SET status = 'paid' WHERE id = 1;

-- back in session A: old snapshot still resolves the prior version
SELECT status FROM orders WHERE id = 1; -- 'pending'
COMMIT;

SELECT status FROM orders WHERE id = 1; -- 'paid'
```

## Visibility rules

A tuple is visible to a reader when all of these hold:

- `xmin == 0` **or** the writer's xid committed before the reader's
  snapshot **and** isn't in the snapshot's in-progress set
- `xmax == 0` **or** the deleter aborted **or** the deleter committed
  after the reader's snapshot

The reader's snapshot records the exact set of in-progress xids at
capture time, so a `BEGIN` in connection A does not leak its writes
into an already-open transaction in connection B until A commits.

## Observability

Every `BEGIN` / `COMMIT` / `ROLLBACK` / savepoint action returns a
status string including the `xid` allocated — useful for correlating
transactions across logs:

```
BEGIN — xid=427 (snapshot isolation)
SAVEPOINT step_1 — sub_xid=428
ROLLBACK TO SAVEPOINT step_1 — aborted 1 sub_xid(s), revived 0 tombstone(s)
COMMIT — xid=427 committed
```

## Isolation levels

`BEGIN` / `START TRANSACTION` accepts an optional `ISOLATION LEVEL`
clause. All accepted modes run under the same snapshot engine today
— the level is a PG compatibility shim:

| Requested level       | Actual semantics                 |
|-----------------------|----------------------------------|
| `READ UNCOMMITTED`    | Snapshot (upgraded — we never expose dirty rows) |
| `READ COMMITTED`      | Snapshot                         |
| `REPEATABLE READ`     | Snapshot (PG maps RR→snapshot too) |
| `SNAPSHOT`            | Snapshot                         |
| _(omitted)_           | Snapshot (default)               |
| `SERIALIZABLE`        | **Rejected** — see below         |

`SERIALIZABLE` is rejected at parse time rather than silently
degraded: real SSI (Serializable Snapshot Isolation with predicate
locking) is a tracked future milestone, and quietly accepting the
keyword while providing weaker guarantees would mislead callers
who depend on the anomaly protection SSI provides. Switch the
statement to `REPEATABLE READ` (or omit the clause) to use the
current snapshot engine.

## Limitations

- The history-store MVCC guarantee applies to SQL table rows first.
  Full multi-model rollout is out of scope for this slice; non-table
  models either use the shared resolver where implemented or retain
  their existing documented behavior.
- `SERIALIZABLE` isolation and SSI are out of scope. RedDB rejects
  `SERIALIZABLE` instead of silently downgrading it.
- Manual `VACUUM` is supported for table-row history and tombstone GC.
  An autovacuum daemon is out of scope.
- Current secondary indexes plus MVCC recheck/fallback provide the
  table-row correctness target. Historical secondary indexes are out
  of scope.
- Prepared transactions, two-phase commit, distributed transaction
  consensus, and cross-node transaction atomicity are out of scope.
- The commit recovery boundary is the complete `TxCommitBatch`.
  In-flight transactions that do not reach that boundary before crash
  are discarded on recovery. Complete committed batches are replayed
  idempotently.

## See also

- [ADR 0014 — MVCC history store and transaction crash recovery](../adr/0014-mvcc-history-store-and-transaction-recovery.md)
- [PRD #432 — MVCC history store and transaction crash recovery](https://github.com/reddb-io/reddb/issues/432)
- [Row Level Security](../security/rls.md)
- [Multi-Tenancy](../security/multi-tenancy.md)
- [WAL & Recovery](../engine/wal.md)
