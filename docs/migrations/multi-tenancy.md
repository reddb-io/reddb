# Multi-Tenant Migrations

RedDB migrations can run with a row-level-security (RLS) tenant context via
`APPLY MIGRATION ... FOR TENANT ...`, but migration status is global today.
There is no per-tenant migration registry yet.

See [Multi-Tenancy](/security/multi-tenancy.md) and
[Row Level Security](/security/rls.md) for background on RedDB's RLS model.

---

## Current contract

`red_migrations` is the only migration status table. When a migration is
applied successfully, its row becomes `status = 'applied'` for the database as
a whole. The same row stores `applied_at`, `rows_processed`, and
`vcs_commit_hash`.

This means tenant-scoped apply is useful for setting the RLS context during a
single execution, but it is not a safe per-tenant rollout tracker. After one
tenant-scoped apply succeeds, later attempts for other tenants will usually see
the migration as already applied.

There is currently no `red_migration_tenants` collection, no per-tenant
`vcs_commit_hash`, and no `ROLLBACK MIGRATION ... FOR TENANT` syntax.

---

## `FOR TENANT <id>`

`FOR TENANT <id>` sets the active RLS tenant context while the migration body
executes:

```sql
APPLY MIGRATION backfill_scores FOR TENANT 'acme-corp';
```

`<id>` may be a string or integer, matching your RLS tenant key.

Use this form only when it is acceptable for the migration to become globally
applied after the execution. For canary-style tenant rollouts, track progress
outside the native migration registry until per-tenant state is implemented.

---

## `FOR TENANT *`

`FOR TENANT *` iterates known tenants and sets the RLS context for each one:

```sql
APPLY MIGRATION backfill_scores FOR TENANT *;
```

Because status is global, this is not a durable per-tenant fanout contract.
The first successful tenant can mark the migration as applied, and subsequent
tenants may be skipped as already applied. The command returns a textual
summary only; it does not persist per-tenant progress.

---

## Schema migrations

Schema changes affect shared collection structure, so apply them without a
tenant clause:

```sql
CREATE MIGRATION add_score_column
AS
  ALTER TABLE users ADD COLUMN score INT DEFAULT 0;

APPLY MIGRATION add_score_column;
```

Adding `FOR TENANT` to a schema migration does not create tenant-local schema.

---

## Data migrations

For data migrations, choose between:

- A global migration that intentionally touches all visible rows.
- A tenant-context migration that executes once under one tenant's RLS context
  and then becomes globally applied.
- An external per-tenant rollout process that registers separate migration
  names per tenant or tracks tenant progress outside `red_migrations`.

Example of separate migration names for explicit tenant canaries:

```sql
CREATE MIGRATION backfill_scores_acme
AS
  UPDATE profiles
  SET score = 0
  WHERE score IS NULL;

APPLY MIGRATION backfill_scores_acme FOR TENANT 'acme-corp';
```

This makes the global registry honest: the applied migration name encodes the
rollout unit.

---

## Rollback

Rollback is global:

```sql
ROLLBACK MIGRATION backfill_scores_acme;
```

The engine reverts the VCS commit stored in `red_migrations.vcs_commit_hash`
and then returns the migration row to `pending`. Tenant-specific rollback is
not implemented; `ROLLBACK MIGRATION <name> FOR TENANT <id>` is not valid
syntax today.
