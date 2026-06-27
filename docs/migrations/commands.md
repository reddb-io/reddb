# Migration Commands — Reference

Complete reference for all four migration commands. See
[Walkthrough](./walkthrough.md) for a guided tour and
[Overview](./overview.md) for concepts.

---

## Syntax grammar

```
migration_stmt :=
    create_migration
  | apply_migration
  | rollback_migration
  | explain_migration

create_migration :=
    CREATE MIGRATION <name>
    [ DEPENDS ON <dep> [, <dep> ...] ]
    [ BATCH <n> ROWS ]
    [ NO ROLLBACK ]
    AS <body>

apply_migration :=
    APPLY MIGRATION ( <name> | * )
    [ FOR TENANT ( <id> | * ) ]

rollback_migration :=
    ROLLBACK MIGRATION <name>

explain_migration :=
    EXPLAIN MIGRATION ( <name> | * )

name   := identifier | quoted_identifier
dep    := identifier | quoted_identifier
n      := positive_integer
body   := sql_statement [ ; sql_statement ... ]
id     := literal_string | integer
```

---

## `CREATE MIGRATION`

Registers a migration. The body is parsed and stored; no SQL in the body is
executed at this point. Dependency inference runs synchronously during
registration — cycle detection errors are returned immediately.

### Required clauses

| Clause | Description |
|---|---|
| `<name>` | Unique identifier for this migration. Must not already exist in `red_migrations`. Identifiers follow standard RQL rules — lowercase, underscores, no spaces. |
| `AS <body>` | One or more SQL statements separated by `;`. The body is stored verbatim. |

### Optional clauses

#### `DEPENDS ON <dep1> [, <dep2> ...]`

Declares explicit prerequisite migrations. The engine adds edges to the
dependency DAG and will refuse to apply this migration until every named
dependency has `status = 'applied'`.

Use explicit `DEPENDS ON` when:
- Auto-inference cannot resolve the dependency unambiguously (two or more prior
  migrations touch the same collection).
- The dependency is on a migration whose body does not mention the shared
  collection by name (e.g., a migration that creates a function or index used
  later).
- You want to make the dependency visible in `EXPLAIN MIGRATION` output even
  when inference would have caught it.

```sql
CREATE MIGRATION add_score_index
DEPENDS ON add_score_column
AS
  CREATE INDEX idx_users_score ON users (score);
```

If a named dependency does not exist in `red_migrations`, the command fails
with:

```
ERROR: unknown migration 'add_score_column' referenced in DEPENDS ON
```

#### `BATCH <n> ROWS`

Marks this migration as a batched data migration. `n` must be a positive
integer. The body must currently be a single idempotent `UPDATE` statement.
When applied, RedDB appends `LIMIT n` and loops until no rows remain,
persisting the `rows_processed` checkpoint after each iteration. Batched
`DELETE` bodies are not supported yet.

See [Data Migrations](./data-migrations.md) for full mechanics.

```sql
CREATE MIGRATION backfill_display_name BATCH 2500 ROWS
AS
  UPDATE users
  SET display_name = first_name || ' ' || last_name
  WHERE display_name IS NULL;
```

#### `NO ROLLBACK`

Marks this migration as irreversible. `ROLLBACK MIGRATION <name>` on this
migration returns an error:

```
ERROR: migration 'drop_legacy_column' is marked NO ROLLBACK and cannot be reverted
```

Use `NO ROLLBACK` for:
- `DROP COLUMN` / `DROP TABLE` statements where the data is intentionally
  discarded.
- Destructive backfills that overwrite source data.
- Any migration where a VCS revert would restore rows that are no longer
  semantically valid.

```sql
CREATE MIGRATION drop_legacy_token_column NO ROLLBACK
AS
  ALTER TABLE sessions DROP COLUMN legacy_token;
```

### Permissions

`CREATE MIGRATION` requires the `Write` role on the database.

### What happens on success

- A row is inserted into `red_migrations` with `status = 'pending'`.
- Dependency edges are inserted into `red_migration_deps` (both inferred and
  explicit).
- Cycle detection runs: if the new edges create a cycle in the DAG, the
  insertion is rolled back and an error is returned.

### Examples

**Minimal schema migration**

```sql
CREATE MIGRATION add_archived_at
AS
  ALTER TABLE posts ADD COLUMN archived_at TIMESTAMP;
```

**Schema migration with explicit dependency**

```sql
CREATE MIGRATION add_archived_index
DEPENDS ON add_archived_at
AS
  CREATE INDEX idx_posts_archived ON posts (archived_at)
  WHERE archived_at IS NOT NULL;
```

**Multi-statement body**

```sql
CREATE MIGRATION extend_user_profile
AS
  ALTER TABLE users ADD COLUMN bio TEXT;
  ALTER TABLE users ADD COLUMN website TEXT;
  ALTER TABLE users ADD COLUMN avatar_url TEXT;
```

**Data migration with batching**

```sql
CREATE MIGRATION normalize_emails BATCH 10000 ROWS
AS
  UPDATE users
  SET email = lower(trim(email))
  WHERE email != lower(trim(email));
```

**Irreversible migration**

```sql
CREATE MIGRATION remove_pii_from_logs NO ROLLBACK
AS
  UPDATE access_logs SET user_agent = NULL, ip_address = NULL;
```

**Batched and irreversible**

```sql
CREATE MIGRATION redact_deleted_users BATCH 1000 ROWS NO ROLLBACK
AS
  UPDATE users
  SET email = NULL, display_name = NULL
  WHERE deleted_at IS NOT NULL AND email IS NOT NULL;
```

---

## `APPLY MIGRATION`

Executes one or all pending migrations. Applied migrations move from
`status = 'pending'` to `status = 'applied'` and a VCS commit is created.

### Applying a named migration

```sql
APPLY MIGRATION add_archived_at;
```

The engine checks that all dependencies of `add_archived_at` are already
`applied`. If any dependency is `pending` or `failed`, the command returns:

```
ERROR: migration 'add_archived_at' has unresolved dependency 'create_posts_table'
```

### Applying all pending migrations

```sql
APPLY MIGRATION *;
```

RedDB performs a topological sort (Kahn's algorithm) over the full pending
set and applies migrations in dependency order. Cycles are impossible at this
point because they were rejected at `CREATE MIGRATION` time.

If any migration fails mid-application, the run stops. Already-applied
migrations in the batch are committed. The failed migration is left with
`status = 'failed'` and the error is recorded. You can inspect it with
`EXPLAIN MIGRATION <name>` and re-attempt with `APPLY MIGRATION <name>` after
fixing the root cause.

### `FOR TENANT <id>`

Sets the row-level-security tenant context before executing the migration body.
The migration sees only rows belonging to `<id>`.

```sql
APPLY MIGRATION backfill_scores FOR TENANT 'tenant-42';
```

`<id>` may be a string or integer, matching the type of your RLS tenant key.

Current limitation: migration status is still global in `red_migrations`.
After a tenant-scoped apply succeeds, the migration is `status = 'applied'`
for the whole database and later attempts for other tenants are treated as
already applied. There is no `red_migration_tenants` per-tenant status table
yet.

### `FOR TENANT *`

Iterates known tenants and sets the RLS context for each tenant in turn.
Because migration status is global today, this is not an independent
per-tenant fanout mechanism: after the first successful tenant applies the
migration, subsequent tenants may see the migration as already applied.
Use this form carefully until per-tenant migration state is implemented.

```sql
APPLY MIGRATION backfill_scores FOR TENANT *;
```

The result is a textual per-tenant summary, not a durable per-tenant progress
table.

### Permissions

`APPLY MIGRATION` requires the `Admin` role on the database.

### What happens on success

- The SQL body is executed.
- `status` is updated to `'applied'`, `applied_at` is set.
- A VCS commit is created. The commit message is:
  `migration: apply <name>`.
- `vcs_commit_hash` is set to the new commit's hash.
- For batched migrations, `rows_processed` is updated incrementally during
  execution.

If the body succeeds but the VCS commit fails, the command returns an error,
records the migration as `failed`, and stores the commit error in
`red_migrations.error`.

### Examples

```sql
-- Apply one migration
APPLY MIGRATION create_accounts_table;

-- Apply all pending in dependency order
APPLY MIGRATION *;

-- Apply with a specific tenant RLS context.
-- Status is still global after success.
APPLY MIGRATION backfill_display_name FOR TENANT 'acme-corp';

-- Iterate known tenants with the same global migration status limitation.
APPLY MIGRATION backfill_display_name FOR TENANT *;
```

---

## `ROLLBACK MIGRATION`

Reverts an applied migration by calling `vcs_revert` on the commit recorded
in `vcs_commit_hash`. This restores the exact data state that existed
immediately before the migration was applied — no compensating SQL is written
or required.

```sql
ROLLBACK MIGRATION add_archived_at;
```

After a successful rollback:
- `status` returns to `'pending'`.
- `applied_at` and `vcs_commit_hash` are cleared.
- The VCS commit is marked reverted in the commit graph.

### Blocked by `NO ROLLBACK`

If the target migration was created with `NO ROLLBACK`, the command returns:

```
ERROR: migration 'remove_pii_from_logs' is marked NO ROLLBACK and cannot be reverted
```

### Blocked by dependents

If another migration depends on the target and is currently `applied`, the
command returns:

```
ERROR: cannot rollback 'add_score_column' - applied migration 'add_score_index' depends on it
```

You must roll back dependents first, in reverse dependency order.

### Permissions

`ROLLBACK MIGRATION` requires the `Admin` role on the database.

### Examples

```sql
-- Roll back a single migration
ROLLBACK MIGRATION add_archived_at;

-- Roll back a chain manually (dependents first)
ROLLBACK MIGRATION add_archived_index;
ROLLBACK MIGRATION add_archived_at;
```

---

## `EXPLAIN MIGRATION`

Returns the current stored migration plan without running it. The current
output is intentionally basic: migration name, status, kind, body, estimated
rows, and lock duration.

```sql
EXPLAIN MIGRATION backfill_scores;
```

### Output fields

| Field | Description |
|---|---|
| `migration` | Migration name |
| `status` | Current status |
| `kind` | `schema` or `data` |
| `body` | The SQL that will be executed |
| `estimated_rows` | Reserved for future planner estimates; currently null |
| `lock_duration_ms` | Reserved for future lock estimates; currently 0 |

### Permissions

`EXPLAIN MIGRATION` is available to any authenticated principal regardless
of role. It does not modify any state.

### Examples

```sql
-- Inspect before applying
EXPLAIN MIGRATION backfill_scores;

-- Check the dependency chain before a bulk apply
EXPLAIN MIGRATION *;
```

`EXPLAIN MIGRATION *` returns the full topological order of all pending
migrations using the same output fields.

---

## Error reference

| Error | Cause |
|---|---|
| `migration '<name>' already exists` | `CREATE MIGRATION` with a name already in `red_migrations` |
| `unknown migration '<name>' referenced in DEPENDS ON` | Named dependency does not exist |
| `cycle detected: <a> → <b> → ... → <a>` | New edges would create a DAG cycle |
| `migration '<name>' has unresolved dependency '<dep>'` | Dependency is `pending` or `failed` |
| `migration '<name>' is marked NO ROLLBACK` | `ROLLBACK MIGRATION` on a `NO ROLLBACK` migration |
| `cannot rollback '<name>' - applied migration '<dep>' depends on it` | Dependent migration is still applied |
| `migration '<name>' not found` | Any command referencing a non-existent migration name |
| `insufficient privileges — Admin role required` | `APPLY` or `ROLLBACK` without Admin role |
| `insufficient privileges — Write role required` | `CREATE MIGRATION` without Write role |
