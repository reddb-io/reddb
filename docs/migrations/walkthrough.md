# Native Migrations — Walkthrough

End-to-end tutorial. You will create a table, register two migrations with a
dependency between them, apply them, inspect the system state, roll back one,
and re-apply. Every statement is copy-paste runnable.

**Prerequisites**: a running RedDB instance and a session with Admin role.
For a local setup:

```bash
cargo build --release --bin red
./target/release/red --path /tmp/walkthrough.rdb
```

Or with Docker:

```bash
docker run -it --rm -p 5432:5432 reddb/reddb:latest
psql -h localhost -U admin -d reddb
```

---

## Step 1: Create a base table

Start with a `users` table. This is ordinary DDL — not a migration, just
the initial schema you will migrate from.

```sql
CREATE TABLE users (
  id       BIGINT PRIMARY KEY,
  email    TEXT NOT NULL,
  name     TEXT NOT NULL,
  status   TEXT NOT NULL DEFAULT 'active',
  created_at TIMESTAMP NOT NULL DEFAULT now()
);
```

Insert some rows so data migrations have something to work with:

```sql
INSERT INTO users (id, email, name) VALUES
  (1, 'alice@example.com', 'Alice'),
  (2, 'BOB@EXAMPLE.COM',   'Bob'),
  (3, 'carol@example.com', 'Carol');
```

---

## Step 2: Register the first migration

Add a `verified_at` column that will be backfilled in the next migration.

```sql
CREATE MIGRATION add_verified_at
AS
  ALTER TABLE users ADD COLUMN verified_at TIMESTAMP;
```

RedDB responds:

```
migration 'add_verified_at' registered (status: pending)
```

Confirm the row exists:

```sql
SELECT name, status, no_rollback, batch_size
FROM red_migrations
WHERE name = 'add_verified_at';
```

```
 name             | status  | no_rollback | batch_size
------------------+---------+-------------+------------
 add_verified_at  | pending | false       | null
```

---

## Step 3: Register the second migration with a dependency

Now register a data migration that backfills `verified_at` from `created_at`.
Because `add_verified_at` is the only prior migration that touches `users`,
RedDB infers the dependency automatically.

```sql
CREATE MIGRATION backfill_verified_at BATCH 1000 ROWS
AS
  UPDATE users
  SET verified_at = created_at
  WHERE verified_at IS NULL;
```

RedDB responds:

```
migration 'backfill_verified_at' registered (status: pending)
auto-inferred dependency: backfill_verified_at → add_verified_at
```

Inspect the dependency table:

```sql
SELECT migration_id, depends_on_id, inferred
FROM red_migration_deps;
```

```
 migration_id          | depends_on_id    | inferred
-----------------------+------------------+---------
 backfill_verified_at  | add_verified_at  | true
```

---

## Step 4: Inspect before applying

Use `EXPLAIN MIGRATION` to preview what will happen:

```sql
EXPLAIN MIGRATION backfill_verified_at;
```

```
name              : backfill_verified_at
status            : pending
batch_size        : 1000
no_rollback       : false
dependency_chain  : [add_verified_at]
estimated_rows    : 3

dependencies:
  - add_verified_at  (pending, inferred)

body:
  UPDATE users SET verified_at = created_at WHERE verified_at IS NULL;

execution_plan:
  [sequential scan on users, filter: verified_at IS NULL, rows=3]
```

`dependency_chain` shows that `backfill_verified_at` cannot be applied until
`add_verified_at` is applied first.

---

## Step 5: Try to apply out of order (and see the error)

Attempt to apply the second migration before the first:

```sql
APPLY MIGRATION backfill_verified_at;
```

```
ERROR: migration 'backfill_verified_at' has unresolved dependency 'add_verified_at'
```

Good. The engine enforced the ordering.

---

## Step 6: Apply all pending migrations in order

```sql
APPLY MIGRATION *;
```

RedDB performs a topological sort and applies in dependency order:

```
applying add_verified_at...
  ok — vcs commit: a3f91c2d...
applying backfill_verified_at...
  batch 1/1 — 3 rows processed
  ok — vcs commit: b72e4401...

2 migrations applied.
```

Verify the schema change:

```sql
SELECT id, email, verified_at FROM users;
```

```
 id | email              | verified_at
----+--------------------+---------------------
  1 | alice@example.com  | 2026-04-01 09:00:00
  2 | BOB@EXAMPLE.COM    | 2026-04-01 09:00:01
  3 | carol@example.com  | 2026-04-01 09:00:02
```

Verify migration status:

```sql
SELECT name, status, applied_at, vcs_commit_hash
FROM red_migrations
ORDER BY applied_at;
```

```
 name                  | status  | applied_at              | vcs_commit_hash
-----------------------+---------+-------------------------+------------------
 add_verified_at       | applied | 2026-05-01 10:01:03.000 | a3f91c2d...
 backfill_verified_at  | applied | 2026-05-01 10:01:03.012 | b72e4401...
```

---

## Step 7: Try to roll back out of order (and see the error)

If you try to roll back `add_verified_at` while `backfill_verified_at` is
still applied, the engine blocks it:

```sql
ROLLBACK MIGRATION add_verified_at;
```

```
ERROR: cannot rollback 'add_verified_at' — applied migration 'backfill_verified_at' depends on it
```

---

## Step 8: Roll back in correct order

Roll back the dependent migration first:

```sql
ROLLBACK MIGRATION backfill_verified_at;
```

```
rolling back backfill_verified_at (vcs_revert b72e4401...)
  ok — migration reset to pending
```

Now roll back the schema migration:

```sql
ROLLBACK MIGRATION add_verified_at;
```

```
rolling back add_verified_at (vcs_revert a3f91c2d...)
  ok — migration reset to pending
```

Verify the schema was restored:

```sql
SELECT column_name FROM information_schema.columns
WHERE table_name = 'users';
```

```
 column_name
-------------
 id
 email
 name
 status
 created_at
```

`verified_at` is gone. The rows are back to their original state. No
compensating SQL was needed.

Check migration status:

```sql
SELECT name, status, applied_at, vcs_commit_hash
FROM red_migrations;
```

```
 name                  | status  | applied_at | vcs_commit_hash
-----------------------+---------+------------+-----------------
 add_verified_at       | pending | null       | null
 backfill_verified_at  | pending | null       | null
```

Both migrations are pending again, ready to be re-applied.

---

## Step 9: Re-apply

```sql
APPLY MIGRATION *;
```

```
applying add_verified_at...
  ok — vcs commit: c90d5512...
applying backfill_verified_at...
  batch 1/1 — 3 rows processed
  ok — vcs commit: d481aa73...

2 migrations applied.
```

Note that the VCS commit hashes are new — each application creates a new
commit, so you have a full history of every application and rollback.

---

## Step 10: Inspect VCS history

```sql
SELECT hash, message, committed_at
FROM red_vcs_commits
ORDER BY committed_at DESC
LIMIT 6;
```

```
 hash         | message                              | committed_at
--------------+--------------------------------------+---------------------
 d481aa73...  | migration: apply backfill_verified_at | 2026-05-01 10:05:01
 c90d5512...  | migration: apply add_verified_at      | 2026-05-01 10:05:00
 b72e4401...  | migration: apply backfill_verified_at | 2026-05-01 10:01:03
 a3f91c2d...  | migration: apply add_verified_at      | 2026-05-01 10:01:03
```

Every application is a named commit. You can time-travel to any point:

```sql
SELECT id, email, verified_at
FROM users
AS OF COMMIT 'a3f91c2d...'
LIMIT 3;
```

```
 id | email              | verified_at
----+--------------------+-------------
  1 | alice@example.com  | null
  2 | BOB@EXAMPLE.COM    | null
  3 | carol@example.com  | null
```

This is the state immediately after `add_verified_at` was applied but before
`backfill_verified_at` ran — the column exists but is null.

---

## What you covered

- `CREATE MIGRATION` — registering schema and data migrations
- `DEPENDS ON` — how auto-inference works and when it fires
- `EXPLAIN MIGRATION` — inspecting the plan before applying
- `APPLY MIGRATION *` — topological application of all pending migrations
- `ROLLBACK MIGRATION` — VCS-backed rollback and the dependent-first ordering
- `red_migrations` and `red_migration_deps` — querying system state directly
- VCS time-travel — `AS OF COMMIT` on a specific migration commit

**Next steps:**

- [Data Migrations](./data-migrations.md) — batching, checkpoints, `NO ROLLBACK`
- [Dependency Graph](./dependency-graph.md) — manual vs inferred deps, cycle detection
- [Cookbook](./cookbook.md) — recipes for renaming columns, NOT NULL backfills, and more
