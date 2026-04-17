# Transactions & MVCC

RedDB implements PostgreSQL-style transactions with snapshot isolation
and full MVCC visibility. Every connection can open a transaction,
nest savepoints, and roll back to any level without affecting other
connections.

## Quick reference

| Statement | Effect |
|-----------|--------|
| `BEGIN` / `START TRANSACTION` | Open a transaction; allocate `xid` |
| `COMMIT` | Make all writes visible; drain tombstones |
| `ROLLBACK` | Discard writes; revive tombstoned rows |
| `SAVEPOINT name` | Push a sub-transaction level |
| `RELEASE SAVEPOINT name` | Pop savepoint (work survives) |
| `ROLLBACK TO SAVEPOINT name` | Abort sub-xids above the savepoint |

## Basic usage

```sql
BEGIN;
INSERT INTO users (id, email) VALUES (1, 'a@b');
UPDATE users SET email = 'x@y' WHERE id = 1;
COMMIT;
```

If the application crashes or the connection drops before `COMMIT`,
every write is rolled back automatically because the WAL never sees
the commit record.

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

`DELETE` inside a transaction is a two-phase operation:

1. The row's `xmax` is stamped with the current writing xid. Other
   active snapshots still see the row.
2. On `COMMIT`, the row is physically removed and CDC emits the Delete
   event.
3. On `ROLLBACK` (or `ROLLBACK TO SAVEPOINT`), `xmax` is wiped back to
   0 so the row reappears for every snapshot that opens after the
   rollback.

Autocommit `DELETE` (without `BEGIN`) still physically removes the row
immediately — no tombstone overhead for one-shot deletes.

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

## Limitations

- `UPDATE` overwrites the tuple in place rather than writing a new
  version. A `ROLLBACK TO SAVEPOINT` after an UPDATE cannot restore
  the pre-update value. INSERT and DELETE are fully reversible.
- `SERIALIZABLE` isolation is accepted by the parser but downgrades to
  `SNAPSHOT ISOLATION` semantics. Predicate locking for true
  serializability is planned.
- Phase 2.3 snapshots are in-process; crash recovery of in-flight
  transactions arrives with Phase 4.

## See also

- [Row Level Security](../security/rls.md)
- [Multi-Tenancy](../security/multi-tenancy.md)
- [WAL & Recovery](../engine/wal.md)
