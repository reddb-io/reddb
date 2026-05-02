# Multi-Tenant Migrations

RedDB's row-level security (RLS) and the migration system are integrated
directly. You can apply a migration to a single tenant, fan it out across
all tenants, or run schema migrations globally while scoping data migrations
per-tenant — all from SQL.

See [Multi-Tenancy](/security/multi-tenancy.md) and
[Row Level Security](/security/rls.md) for background on RedDB's RLS model.

---

## `FOR TENANT <id>`

`FOR TENANT <id>` sets the RLS tenant context for the duration of the
migration's execution. The migration body sees only rows belonging to that
tenant — inserts carry the tenant key automatically, and reads are scoped
by the RLS policy.

```sql
APPLY MIGRATION backfill_scores FOR TENANT 'acme-corp';
```

What happens:

1. The RLS context is set to `tenant_id = 'acme-corp'`.
2. The migration body executes. Every `SELECT`, `INSERT`, `UPDATE`, and
   `DELETE` is automatically filtered by the RLS policy.
3. The context is cleared after execution.
4. A VCS commit is created with message:
   `migration: apply backfill_scores tenant acme-corp`

The `vcs_commit_hash` in `red_migrations` records this specific tenant's
commit.

### Tenant ID types

`<id>` may be a string or integer, matching the type of your RLS tenant key:

```sql
-- String tenant key
APPLY MIGRATION backfill_profiles FOR TENANT 'tenant-42';

-- Integer tenant key
APPLY MIGRATION backfill_profiles FOR TENANT 42;
```

---

## `FOR TENANT *`

`FOR TENANT *` fans out the migration to every tenant in the tenant registry.
RedDB iterates the tenant list, applies the migration for each tenant
sequentially (with RLS context set per iteration), and tracks progress
independently.

```sql
APPLY MIGRATION backfill_scores FOR TENANT *;
```

Output:

```
fanning out to 47 tenants...
  tenant acme-corp      — ok (vcs: c8a3...) — 1,240 rows
  tenant beta-inc       — ok (vcs: d912...) — 830 rows
  tenant gamma-llc      — failed: constraint violation on row id=5029
  tenant delta-co       — ok (vcs: e017...) — 2,100 rows
  ...

47 tenants processed. 46 ok, 1 failed.
Failed tenants: gamma-llc
```

Failures in one tenant do not stop the fanout. All other tenants continue
processing. Failed tenants are listed at the end.

To retry a failed tenant:

```sql
APPLY MIGRATION backfill_scores FOR TENANT 'gamma-llc';
```

---

## Schema migrations vs. data migrations in multi-tenant setups

### Schema migrations: apply globally, not per-tenant

Schema changes (adding a column, creating an index, altering a type) affect
the collection structure, which is shared across all tenants. Apply schema
migrations without a `FOR TENANT` clause:

```sql
-- Correct: schema change is global
APPLY MIGRATION add_score_column;

-- Incorrect: FOR TENANT on a schema migration is redundant
-- and may produce unexpected behavior on non-targeted tenants
APPLY MIGRATION add_score_column FOR TENANT 'acme-corp';
```

When you add a column without `FOR TENANT`, all tenants immediately see the
new column (with its default value or null). No per-tenant application is
needed.

### Data migrations: apply per-tenant for isolation

Data migrations (backfills, normalizations, purges) operate on rows. Because
RLS isolates rows by tenant, applying a data migration without `FOR TENANT`
executes with a superuser context that bypasses RLS and touches all tenants'
rows in one pass.

Depending on your RLS policy, this may be correct (all tenants get identical
treatment) or incorrect (you want per-tenant isolation for audit, progress
tracking, or partial rollout).

Use `FOR TENANT *` for the safe, auditable path:

```sql
-- Each tenant gets its own VCS commit and independent progress tracking
APPLY MIGRATION backfill_display_names FOR TENANT *;
```

Use the global path only when:
- The migration logic is identical for all tenants and you want a single
  commit in history.
- You explicitly need to cross tenant boundaries (super-admin operation).

---

## Partial rollout pattern

You can use `FOR TENANT <id>` to roll out a data migration to a subset of
tenants before committing to a full fanout. This is useful for validating
correctness on a representative tenant before touching all tenants.

```sql
-- Step 1: Apply to one tenant as a canary
APPLY MIGRATION backfill_scores FOR TENANT 'canary-tenant';

-- Step 2: Verify results
SELECT avg(score), count(*) FROM profiles
WHERE tenant_id = 'canary-tenant';

-- Step 3: Fan out to all remaining tenants
APPLY MIGRATION backfill_scores FOR TENANT *;
-- RedDB skips tenants that are already applied
```

When `FOR TENANT *` is run after a partial rollout, tenants where the
migration was already applied (via a prior `FOR TENANT <id>`) are skipped
automatically — they remain `status = 'applied'` and are not re-executed.

---

## Tracking per-tenant migration status

`red_migrations` stores the global status of each migration. For multi-tenant
fanout, per-tenant status is tracked in a companion system collection:
`red_migration_tenants`.

```sql
SELECT
  migration_name,
  tenant_id,
  status,
  rows_processed,
  applied_at,
  vcs_commit_hash
FROM red_migration_tenants
WHERE migration_name = 'backfill_scores'
ORDER BY applied_at;
```

```
 migration_name  | tenant_id   | status  | rows_processed | applied_at
-----------------+-------------+---------+----------------+---------------------
 backfill_scores | acme-corp   | applied | 1240           | 2026-05-01 10:05:01
 backfill_scores | beta-inc    | applied | 830            | 2026-05-01 10:05:03
 backfill_scores | gamma-llc   | failed  | 400            | 2026-05-01 10:05:07
 backfill_scores | delta-co    | applied | 2100           | 2026-05-01 10:05:09
```

For batched migrations, `rows_processed` is updated per-tenant after each
batch, giving you per-tenant progress tracking.

---

## RLS interaction with migration execution

During migration execution, the RLS policy applies to every statement in the
body. This means:

- `UPDATE users SET score = 0 WHERE score IS NULL` — with `FOR TENANT 'acme-corp'`
  set, this touches only `acme-corp` rows. Rows belonging to other tenants
  are invisible.
- `INSERT INTO profiles (user_id, score) VALUES (...)` — the `tenant_id`
  column is automatically populated from the RLS context. You do not need to
  include it in the migration body.

If your migration body explicitly sets `tenant_id`, it must match the
active RLS context — the engine rejects cross-tenant writes:

```
ERROR: RLS violation: cannot insert row with tenant_id 'other-tenant' while
       RLS context is set to 'acme-corp'
```

---

## Rollback for tenant-scoped migrations

`ROLLBACK MIGRATION <name>` without a tenant clause reverts the global
migration state (only applicable for schema migrations applied globally).

For tenant-scoped migrations, rollback must specify the tenant:

```sql
-- Roll back for a specific tenant
ROLLBACK MIGRATION backfill_scores FOR TENANT 'gamma-llc';
```

This calls `vcs_revert` on the commit recorded in `red_migration_tenants`
for that tenant, restoring only that tenant's rows to their pre-migration
state. Other tenants are unaffected.

`ROLLBACK MIGRATION <name> FOR TENANT *` rolls back all tenants that have
`status = 'applied'` for that migration, in reverse application order.

---

## Pattern: coordinating schema and data migrations in a multi-tenant system

```sql
-- Step 1: Schema migration — no tenant scope, affects all tenants
CREATE MIGRATION add_tier_column
AS
  ALTER TABLE accounts ADD COLUMN tier TEXT NOT NULL DEFAULT 'free';

APPLY MIGRATION add_tier_column;

-- Step 2: Data migration — per-tenant scope for isolation
CREATE MIGRATION backfill_paid_tiers BATCH 1000 ROWS
DEPENDS ON add_tier_column
AS
  UPDATE accounts
  SET tier = 'paid'
  WHERE subscription_status = 'active';

-- Apply to a canary tenant first
APPLY MIGRATION backfill_paid_tiers FOR TENANT 'canary-corp';

-- Validate
SELECT tier, count(*) FROM accounts
WHERE tenant_id = 'canary-corp'
GROUP BY tier;

-- Fan out to all
APPLY MIGRATION backfill_paid_tiers FOR TENANT *;
```
