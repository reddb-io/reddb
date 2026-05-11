# Native SQL Migrations — Overview

RedDB ships a first-class migration system built directly into the query engine.
You write SQL, you run SQL, and migrations live alongside your data — versioned,
auditable, and resumable — without installing a second tool, maintaining a
separate config file, or wiring up a migration runner in your deploy pipeline.

---

## The problem with external migration tools

External tools — Flyway, Liquibase, Drizzle Migrate, Sequelize Migrate — solve
a real problem, but they introduce friction that compounds over time:

**They are out-of-band.** Your migrations live in a different process, a
different config, and often a different language from your database. You have to
keep them in sync with schema changes manually. When a migration fails at 3 AM,
you are reading two separate logs from two separate systems to reconstruct what
happened.

**They do not understand your data.** Flyway applies SQL in file-name order. It
has no knowledge of which migrations touch overlapping tables, which should run
before which, or which depend on data that an earlier migration was supposed to
create. You encode that knowledge in filename prefixes like `V2026_04_01__` and
hope nobody commits a file out of order.

**They do not know about your branches.** When two engineers each merge a feature
branch with a migration on the same day, external tools have no way to detect
the conflict. The lexicographic ordering wins — and if that ordering is wrong,
your schema is wrong.

**They cannot resume interrupted work.** If a data migration fails halfway through
backfilling 50 million rows, external tools leave you to figure out where to
restart. You either re-run the whole migration (slow, risky on live data) or
hand-craft a resume query.

**They live outside your audit trail.** Your database records what data changed
but not what migration caused the change. Reproducing the exact state that
existed during an incident requires correlating migration logs, VCS commits, and
deploy records from separate systems.

---

## What RedDB's native migrations give you

### One language, one system

Migrations are SQL statements executed by the same engine that runs your queries.
There is no migration runner binary to install, no JDBC driver to configure, no
YAML manifest to maintain alongside your SQL. You author a migration in a SQL
session and apply it in a SQL session.

```sql
CREATE MIGRATION add_verified_at
AS
  ALTER TABLE users ADD COLUMN verified_at TIMESTAMP;
```

That migration is now stored in the `red_migrations` system collection and is
immediately available for inspection, dependency analysis, and application.

### VCS-native: one commit per applied migration

Every `APPLY MIGRATION` creates a VCS commit in RedDB's built-in version control
layer. The commit hash is recorded in `red_migrations.vcs_commit_hash`. Rolling
back a migration does not run compensating SQL — it calls `vcs_revert` on the
commit, restoring the exact data state that existed before the migration ran.

This means rollback is exact and instantaneous for schema migrations and
data migrations alike, regardless of how many rows were affected.

### Automatic dependency inference

When you register a migration, RedDB scans the body for `FROM`, `INTO`, `TABLE`,
`UPDATE`, `JOIN`, and `ON` keywords to find which collections the migration
touches. If exactly one previously registered migration touches the same
collection, a dependency edge is created automatically. You only need to write
explicit `DEPENDS ON` clauses when inference is ambiguous.

```sql
-- RedDB infers that this depends on add_verified_at because
-- both touch the users collection and add_verified_at is the
-- only prior migration that does.
CREATE MIGRATION backfill_verified_at
AS
  UPDATE users SET verified_at = created_at WHERE verified_at IS NULL;
```

### Checkpoint resume for data migrations

`BATCH N ROWS` turns a data migration into a restartable loop. Each iteration
RedDB appends `LIMIT N` to the body, runs it, persists the running
`rows_processed` checkpoint into `red_migrations`, and stops when the iteration
reports fewer than `N` rows affected. An interrupted migration is safe to
re-`APPLY`: the next run picks up where the last one stopped.

The mechanism leans on the migration body's `WHERE` clause being **idempotent
under replay** — written so rows already processed are filtered out next time.
The example below satisfies this naturally: every applied row sets
`score IS NULL` to a non-null value, so subsequent batches only see still-null
rows. The contract is operator-owned; pinned by a regression test
(`tests/chaos_migration_batch_resume.rs`) that runs the same `UPDATE` three
times back-to-back and asserts the second and third are no-ops.

```sql
CREATE MIGRATION backfill_scores BATCH 5000 ROWS
AS
  UPDATE profiles
  SET score = calculate_score(activity_count, join_date)
  WHERE score IS NULL;
```

> **Stretch goal — SIGKILL drill.** A real chaos test that kills the engine
> mid-batch via `SIGKILL`, restarts, and asserts no double-apply requires
> subprocess-management infrastructure that doesn't yet live in the test
> harness. The in-process resume + idempotent-replay tests today guard the
> load-bearing pieces; the SIGKILL drill stays open as future work tied to the
> broader chaos infra.

### VCS application scope

Every applied migration creates a VCS commit, but migration definitions are not
branch-local today. Branch-scoped migration visibility is not implemented:
`red_migrations` is a global system collection, so a migration registered on a
feature branch is visible after checking out another branch.

Because migration definitions are global, RedDB does not currently block
`red vcs merge` with a migration-specific schema conflict. Coordinate branch
ordering with explicit `DEPENDS ON` edges after both migration definitions exist.

### Multi-tenant fanout

`FOR TENANT *` applies a migration to every known tenant in a single statement,
setting the row-level-security context before each execution. Per-tenant progress
is tracked individually so a failure in one tenant does not block the others.

### Irreversibility is explicit

`NO ROLLBACK` marks a migration as intentionally one-way. The engine blocks
`ROLLBACK MIGRATION` on that name, surfacing a clear error rather than silently
doing nothing or applying a broken compensating migration.

---

## Comparison table

| Capability | RedDB | Flyway | Liquibase | Drizzle Migrate | Sequelize Migrate |
|---|---|---|---|---|---|
| **Written in SQL** | yes | yes | XML/YAML/SQL | TypeScript | JavaScript |
| **Separate process required** | no | yes | yes | yes | yes |
| **Dependency graph (DAG)** | yes, auto-inferred | no | no | no | no |
| **Cycle detection** | yes, at CREATE time | no | no | no | no |
| **Checkpoint resume on crash** | yes (`BATCH N ROWS`) | no | no | no | no |
| **Rollback mechanism** | VCS revert (exact) | compensating SQL | compensating SQL | manual | manual |
| **Irreversible migration guard** | yes (`NO ROLLBACK`) | no | no | no | no |
| **VCS commit per migration** | yes | no | no | no | no |
| **Branch-scoped application** | no, definitions are global | no | no | no | no |
| **Multi-tenant fanout** | yes (`FOR TENANT *`) | no | no | no | no |
| **Stored in database** | yes (`red_migrations`) | separate table | separate table | separate table | separate table |
| **Inspect via SQL** | yes | limited | limited | no | no |
| **EXPLAIN before apply** | yes | no | dry-run only | no | no |

---

## System collections

RedDB stores migration state in two system collections. You can query them
directly with `SELECT`.

**`red_migrations`** — one row per registered migration.

| Field | Type | Description |
|---|---|---|
| `name` | TEXT | Unique migration name |
| `status` | TEXT | `pending`, `applied`, or `failed` |
| `kind` | TEXT | `schema` or `data` |
| `body` | TEXT | Original SQL body |
| `author` | TEXT | Session principal at CREATE time |
| `created_at` | TIMESTAMP | When the migration was registered |
| `applied_at` | TIMESTAMP | When it was last applied (null if pending) |
| `rows_total` | BIGINT | Estimated total rows (data migrations) |
| `rows_processed` | BIGINT | Checkpoint cursor (data migrations) |
| `vcs_commit_hash` | TEXT | Commit hash created by the last APPLY |
| `no_rollback` | BOOLEAN | Whether ROLLBACK MIGRATION is blocked |
| `batch_size` | BIGINT | Batch size (null for non-batched migrations) |

**`red_migration_deps`** — one row per dependency edge.

| Field | Type | Description |
|---|---|---|
| `migration_id` | TEXT | Dependent migration name |
| `depends_on_id` | TEXT | Prerequisite migration name |
| `inferred` | BOOLEAN | `true` if edge was auto-inferred, `false` if explicit |

---

## Documentation map

- [Command Reference](./commands.md) — full syntax and options for all four migration commands
- [Walkthrough](./walkthrough.md) — end-to-end tutorial from scratch
- [Data Migrations](./data-migrations.md) — `BATCH N ROWS`, checkpoint resume, `NO ROLLBACK`
- [Dependency Graph](./dependency-graph.md) — DAG management, auto-inference, cycle detection
- [VCS Integration](./vcs-integration.md) — commits, rollback via revert, current VCS scope
- [Multi-Tenancy](./multi-tenancy.md) — `FOR TENANT`, RLS context, fanout patterns
- [Cookbook](./cookbook.md) — recipes for common real-world migration patterns
