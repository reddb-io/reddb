# VCS Integration

Every applied migration creates a VCS commit. This is not a log entry or
an audit row — it is a full commit in RedDB's version-control layer,
carrying the same semantics as any other VCS commit: it is immutable,
content-addressed, and can be reverted, diffed against, or used as a
time-travel target.

See [Git for Data — Overview](/vcs/overview.md) for background on RedDB's
VCS layer.

---

## What happens when you apply a migration

When `APPLY MIGRATION <name>` succeeds:

1. The SQL body executes inside a transaction.
2. On commit, the engine calls `vcs.commit` with the message:
   `migration: apply <name>`
   (or `migration: apply <name> tenant <id>` for tenant-scoped applications).
3. The resulting commit hash is written to `red_migrations.vcs_commit_hash`.
4. `status` is updated to `'applied'` and `applied_at` is set.

All of this happens atomically — either the SQL, the VCS commit, and the
status update all succeed, or none of them do.

### Commit message format

| Scenario | Commit message |
|---|---|
| Plain apply | `migration: apply add_verified_at` |
| Tenant-scoped | `migration: apply backfill_scores tenant acme-corp` |
| Tenant fanout (per-tenant commit) | `migration: apply backfill_scores tenant acme-corp` |

Each tenant in a `FOR TENANT *` fanout gets its own commit with the
tenant ID in the message.

### Reading the commit hash

```sql
SELECT name, vcs_commit_hash, applied_at
FROM red_migrations
WHERE status = 'applied'
ORDER BY applied_at;
```

```
 name                  | vcs_commit_hash | applied_at
-----------------------+-----------------+---------------------
 add_verified_at       | a3f91c2d...     | 2026-05-01 10:01:03
 backfill_verified_at  | b72e4401...     | 2026-05-01 10:01:03
```

---

## Rollback via VCS revert

`ROLLBACK MIGRATION <name>` calls `vcs_revert` on the commit stored in
`vcs_commit_hash`. This restores the exact data state that existed at the
snapshot immediately before that commit.

There is no compensating SQL. There is no "down migration" file to maintain.
The VCS layer handles it entirely.

```sql
ROLLBACK MIGRATION backfill_verified_at;
```

What happens:

1. The engine reads `vcs_commit_hash` from `red_migrations`.
2. `vcs_revert` is called, which creates a revert commit that undoes the
   original commit's changes.
3. `status` is reset to `'pending'`, `applied_at` and `vcs_commit_hash` are
   cleared.

The revert commit is itself recorded in the VCS log, so you have full
history: the original apply commit, then the revert commit.

### Viewing the revert in VCS history

```sql
SELECT hash, message, committed_at
FROM red_vcs_commits
ORDER BY committed_at DESC
LIMIT 6;
```

```
 hash         | message                                      | committed_at
--------------+----------------------------------------------+---------------------
 e5c2881f...  | revert: migration: apply backfill_verified_at | 2026-05-01 10:03:00
 b72e4401...  | migration: apply backfill_verified_at         | 2026-05-01 10:01:03
 a3f91c2d...  | migration: apply add_verified_at              | 2026-05-01 10:01:03
```

---

## Time-travel to a pre-migration state

Because each migration creates a commit, you can query the exact state
of a collection at any point in the migration history:

```sql
-- State before backfill_verified_at ran (column exists, values are null)
SELECT id, verified_at
FROM users
AS OF COMMIT 'a3f91c2d...'
LIMIT 5;
```

```
 id | verified_at
----+-------------
  1 | null
  2 | null
  3 | null
```

```sql
-- State after backfill_verified_at ran (column populated)
SELECT id, verified_at
FROM users
AS OF COMMIT 'b72e4401...'
LIMIT 5;
```

```
 id | verified_at
----+---------------------
  1 | 2026-04-01 09:00:00
  2 | 2026-04-01 09:00:01
  3 | 2026-04-01 09:00:02
```

This is useful for incident investigation: you can pinpoint exactly which
migration introduced a data state and query against the exact snapshot.

You can also reference the commit by a branch or tag if you tagged it:

```sql
-- Tag the commit after a release
red vcs tag v2.3.0 b72e4401

-- Later, query against that tag
SELECT count(*) FROM users AS OF TAG 'v2.3.0';
```

---

## Branch-scoped migrations

Migrations are applied on the current VCS branch. If you are on a feature
branch, migration commits land on that branch's history. The `main` branch
is unaffected until you merge.

### Workflow

```sql
-- Create a feature branch
-- (from CLI or via VCS commands)
```

```bash
red vcs branch feature/add-scores
red vcs checkout feature/add-scores
```

```sql
-- Register and apply on the feature branch
CREATE MIGRATION add_score_column
AS
  ALTER TABLE users ADD COLUMN score INT NOT NULL DEFAULT 0;

APPLY MIGRATION add_score_column;
```

The commit `migration: apply add_score_column` exists only on
`feature/add-scores`. If you query `main`, the `score` column does not exist.

### Merge and conflict detection

When you merge the feature branch into `main`:

```bash
red vcs merge feature/add-scores
```

RedDB checks whether any migration applied on the feature branch conflicts
with migrations applied on `main` since the divergence point. A conflict
exists if:

- Both branches applied migrations that modify the same collection's schema
  (e.g., both added a column named `score`).
- The topological ordering of migrations from both branches would create an
  inconsistency.

On conflict, the merge is blocked and a conflict set is returned:

```
MERGE CONFLICT: migration 'add_score_column' on feature/add-scores conflicts
with 'add_score_field' on main (both modify collection 'users').

Resolve with:
  GET /repo/merges/<msid>/conflicts
  POST /repo/merges/<msid>/conflicts/<cid>/resolve
```

You resolve the conflict by choosing which migration wins, or by creating
a new migration that reconciles both changes, then completing the merge.

### Why this matters

Without branch-scoped migrations, two engineers merging on the same day can
silently corrupt the migration ordering. With branch scoping, the conflict
is detected before data is affected and you choose the resolution
intentionally.

---

## Inspecting migration history via VCS log

```bash
# Show all migration commits
red vcs log | grep "migration:"
```

```
commit a3f91c2d... migration: apply add_verified_at
commit b72e4401... migration: apply backfill_verified_at
commit c90d5512... migration: apply add_verified_at
commit d481aa73... migration: apply backfill_verified_at
```

You can also diff between two migration states:

```bash
red vcs diff a3f91c2d b72e4401
```

```
collection: users
  row 1: verified_at null → 2026-04-01 09:00:00
  row 2: verified_at null → 2026-04-01 09:00:01
  row 3: verified_at null → 2026-04-01 09:00:02
```

---

## Collections that must be VCS-opted-in

For `AS OF COMMIT` queries on collections touched by migrations to work,
those collections must be opted into VCS versioning:

```sql
ALTER TABLE users SET VERSIONED = true;
```

System collections (`red_migrations`, `red_migration_deps`) are versioned
automatically. User collections require explicit opt-in. See
[Git for Data — Walkthrough](/vcs/walkthrough.md) for details.

If you attempt `AS OF COMMIT` on a non-versioned collection, the engine
returns:

```
ERROR: AS OF requires a versioned collection — 'users' has not opted in.
Call ALTER TABLE users SET VERSIONED = true first.
```

---

## VCS integration and `NO ROLLBACK`

When a migration is marked `NO ROLLBACK`, the VCS commit is still created
normally. The commit exists in history and is accessible for time-travel
queries. The engine only blocks the `vcs_revert` call that `ROLLBACK MIGRATION`
would trigger.

This means you can always query:

```sql
SELECT count(*) FROM users
AS OF COMMIT '<hash-of-purge-migration>';
```

to see the state immediately after the migration ran — even if you cannot
roll it back.
