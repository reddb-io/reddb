# Migration Cookbook

Recipes for common real-world migration patterns. Each recipe is a
self-contained problem and solution.

---

## Rename a column safely

**Problem:** You need to rename `user_name` to `display_name` in `users`.
Applications read and write the column. You cannot cut over instantaneously
without downtime.

**Solution:** Add the new column, backfill it, keep both in sync with a
trigger, then drop the old column in a later migration after all application
code has been updated.

**Phase 1: Add the new column**

```sql
CREATE MIGRATION add_display_name
AS
  ALTER TABLE users ADD COLUMN display_name TEXT;
```

**Phase 2: Backfill from the old column**

```sql
CREATE MIGRATION backfill_display_name BATCH 5000 ROWS
DEPENDS ON add_display_name
AS
  UPDATE users
  SET display_name = user_name
  WHERE display_name IS NULL;
```

**Phase 3: Apply phases 1 and 2**

```sql
APPLY MIGRATION add_display_name;
APPLY MIGRATION backfill_display_name;
```

At this point, both columns exist and are in sync. Deploy application code
that writes to `display_name`. Old code still reads and writes `user_name`.

**Phase 4: Drop the old column (after all code is updated)**

```sql
CREATE MIGRATION drop_user_name NO ROLLBACK
DEPENDS ON backfill_display_name
AS
  ALTER TABLE users DROP COLUMN user_name;

APPLY MIGRATION drop_user_name;
```

`NO ROLLBACK` because dropping the column destroys data that cannot be
reconstructed from `display_name` alone (users who wrote to `user_name`
after the backfill would be lost on revert).

---

## Add a NOT NULL column with a backfill

**Problem:** You want to add `status TEXT NOT NULL DEFAULT 'active'` to
`orders`, but the table has 20 million rows and you cannot afford a full
table lock for the duration of a bulk update.

**Solution:** Add the column as nullable, backfill in batches, then add
the `NOT NULL` constraint.

**Step 1: Add column as nullable**

```sql
CREATE MIGRATION add_orders_status
AS
  ALTER TABLE orders ADD COLUMN status TEXT;
```

**Step 2: Backfill in batches**

```sql
CREATE MIGRATION backfill_orders_status BATCH 10000 ROWS
DEPENDS ON add_orders_status
AS
  UPDATE orders
  SET status = 'active'
  WHERE status IS NULL;
```

**Step 3: Add the NOT NULL constraint**

```sql
CREATE MIGRATION constrain_orders_status
DEPENDS ON backfill_orders_status
AS
  ALTER TABLE orders ALTER COLUMN status SET NOT NULL;
  ALTER TABLE orders ALTER COLUMN status SET DEFAULT 'active';
```

**Apply in order:**

```sql
APPLY MIGRATION *;
```

RedDB applies the three migrations in dependency order. The batch step
processes rows in 10,000-row chunks, so the full table lock is never held
for more than one batch at a time.

---

## Populate a new denormalized column in batches

**Problem:** You have `orders` with `user_id` and a `users` table. You want
to add `user_email` to `orders` for fast reads without joining every time.

**Solution:** Add the column, then backfill from the join.

```sql
CREATE MIGRATION add_order_user_email
AS
  ALTER TABLE orders ADD COLUMN user_email TEXT;

CREATE MIGRATION backfill_order_user_email BATCH 5000 ROWS
DEPENDS ON add_order_user_email
AS
  UPDATE orders o
  SET user_email = (
    SELECT email FROM users WHERE id = o.user_id
  )
  WHERE o.user_email IS NULL;

APPLY MIGRATION *;
```

Note that the scanner will detect both `orders` and `users` in the
backfill migration's body (via the subquery `FROM users`). If there are
other migrations that touch `orders` or `users`, you may see an ambiguity
warning and need to add explicit `DEPENDS ON` clauses.

---

## Remove a deprecated table

**Problem:** `legacy_sessions` is no longer used. You want to remove it.
The table has no foreign keys pointing to it. This is irreversible.

**Solution:**

```sql
CREATE MIGRATION drop_legacy_sessions NO ROLLBACK
AS
  DROP TABLE legacy_sessions;

APPLY MIGRATION drop_legacy_sessions;
```

`NO ROLLBACK` because the data is permanently destroyed. If you later
discover the table was still needed, you cannot recover via rollback — you
would need a backup.

**Safer approach if unsure:** archive instead of drop.

```sql
CREATE MIGRATION archive_legacy_sessions NO ROLLBACK
AS
  CREATE TABLE legacy_sessions_archive AS SELECT * FROM legacy_sessions;
  DROP TABLE legacy_sessions;
```

This preserves the data in an archive table while removing the operational
table. The archive can be dropped later once you are confident no data is
needed.

---

## Coordinate migrations across multiple feature branches

**Problem:** Engineer A is on `feature/add-scores` adding a `score` column.
Engineer B is on `feature/add-ranks` adding a `rank` column. Both need to
merge to `main`. `add-ranks` logically depends on `add-scores`.

**Solution:** Engineer B adds an explicit `DEPENDS ON` clause that references
Engineer A's migration name. When both branches are merged, the dependency
is enforced during application.

**Engineer A on `feature/add-scores`:**

```sql
CREATE MIGRATION add_score_column
AS
  ALTER TABLE users ADD COLUMN score INT NOT NULL DEFAULT 0;
```

**Engineer B on `feature/add-ranks`:**

```sql
CREATE MIGRATION add_rank_column
DEPENDS ON add_score_column
AS
  ALTER TABLE users ADD COLUMN rank INT;

CREATE MIGRATION populate_ranks BATCH 2000 ROWS
DEPENDS ON add_rank_column
AS
  UPDATE users
  SET rank = (
    SELECT count(*) + 1 FROM users u2
    WHERE u2.score > users.score
  )
  WHERE rank IS NULL;
```

**After both branches are merged to `main`:**

```sql
APPLY MIGRATION *;
```

RedDB's topological sort resolves the ordering: `add_score_column` runs
first (no deps), then `add_rank_column` (depends on `add_score_column`),
then `populate_ranks` (depends on `add_rank_column`).

If `add_score_column` does not exist when `add_rank_column` is registered
(because Engineer A's branch has not merged yet), `CREATE MIGRATION` with
`DEPENDS ON add_score_column` returns:

```
ERROR: unknown migration 'add_score_column' referenced in DEPENDS ON
```

This is a signal to Engineer B to either wait for the merge or register
the dependency on the fly after the merge.

---

## Emergency rollback procedure

**Problem:** A migration was applied to production and caused a regression.
You need to roll back immediately.

**Step 1: Identify the problematic migration**

```sql
SELECT name, status, applied_at, vcs_commit_hash
FROM red_migrations
WHERE status = 'applied'
ORDER BY applied_at DESC
LIMIT 5;
```

**Step 2: Check if rollback is possible**

```sql
EXPLAIN MIGRATION <name>;
```

Look for:
- `no_rollback: false` — rollback is available.
- `no_rollback: true` — you cannot use `ROLLBACK MIGRATION`. Use a manual
  compensating migration or restore from backup.

**Step 3: Check for dependents**

If later migrations depend on the one you want to roll back, you must roll
back the dependents first:

```sql
-- Find applied migrations that depend on the target
SELECT migration_id, depends_on_id
FROM red_migration_deps
WHERE depends_on_id = '<target_migration>'
AND migration_id IN (
  SELECT name FROM red_migrations WHERE status = 'applied'
);
```

**Step 4: Roll back in reverse dependency order**

```sql
-- Roll back dependents first
ROLLBACK MIGRATION <dependent_migration>;

-- Then roll back the target
ROLLBACK MIGRATION <target_migration>;
```

**Step 5: Verify**

```sql
SELECT name, status FROM red_migrations
WHERE name IN ('<target_migration>', '<dependent_migration>');
```

Both should show `status = 'pending'`.

```sql
-- Verify the data state
SELECT <affected columns> FROM <affected table> LIMIT 10;
```

---

## Split a column into two columns

**Problem:** `full_name TEXT` needs to be split into `first_name TEXT` and
`last_name TEXT`. Names are formatted as "First Last" (single space).

```sql
-- Step 1: Add new columns
CREATE MIGRATION split_full_name_add_columns
AS
  ALTER TABLE users ADD COLUMN first_name TEXT;
  ALTER TABLE users ADD COLUMN last_name TEXT;

-- Step 2: Backfill by splitting on space
CREATE MIGRATION split_full_name_backfill BATCH 5000 ROWS
DEPENDS ON split_full_name_add_columns
AS
  UPDATE users
  SET
    first_name = split_part(full_name, ' ', 1),
    last_name  = split_part(full_name, ' ', 2)
  WHERE first_name IS NULL;

-- Step 3: Add NOT NULL constraints once backfilled
CREATE MIGRATION split_full_name_constrain
DEPENDS ON split_full_name_backfill
AS
  ALTER TABLE users ALTER COLUMN first_name SET NOT NULL;
  ALTER TABLE users ALTER COLUMN last_name  SET NOT NULL;

-- Step 4: Drop the old column (only after all code is migrated)
CREATE MIGRATION split_full_name_drop_old NO ROLLBACK
DEPENDS ON split_full_name_constrain
AS
  ALTER TABLE users DROP COLUMN full_name;

-- Apply steps 1–3 now; defer step 4 until application code no longer reads full_name
APPLY MIGRATION split_full_name_add_columns;
APPLY MIGRATION split_full_name_backfill;
APPLY MIGRATION split_full_name_constrain;
```

---

## Index creation on a large table

**Problem:** You need to add a non-unique index on `events(created_at)`.
The table has 500 million rows. You want to create the index without
blocking reads or writes during creation.

```sql
CREATE MIGRATION add_events_created_index
AS
  CREATE INDEX CONCURRENTLY idx_events_created_at ON events (created_at);
```

`CREATE INDEX CONCURRENTLY` builds the index without holding a lock on
the table. This is slower than a blocking index build but safe for
production.

Because this is a schema change and not a data change, no `BATCH N ROWS`
is needed. The `CONCURRENTLY` modifier handles progress internally.

**Note:** If `CREATE INDEX CONCURRENTLY` is interrupted (process crash,
connection drop), it may leave a partially-built index. Check for invalid
indexes after the migration:

```sql
SELECT indexname, indexdef
FROM pg_indexes
WHERE tablename = 'events'
AND indexname LIKE '%invalid%';
```

If an invalid index exists, drop it manually and re-run the migration.

---

## Backfill from an external source via a temporary staging table

**Problem:** You need to import a CSV of new data into an existing table.
The import is large and should be batched.

```sql
-- Step 1: Create a staging table
CREATE MIGRATION create_import_staging
AS
  CREATE TABLE import_users_staging (
    external_id TEXT PRIMARY KEY,
    email TEXT,
    name TEXT,
    imported_at TIMESTAMP DEFAULT now()
  );

-- Step 2: (Load the CSV into staging — done outside migrations)
-- COPY import_users_staging FROM '/tmp/users_import.csv' CSV HEADER;

-- Step 3: Merge staging into users in batches
CREATE MIGRATION import_users_from_staging BATCH 2000 ROWS
DEPENDS ON create_import_staging
AS
  INSERT INTO users (email, name)
  SELECT email, name FROM import_users_staging
  WHERE external_id NOT IN (SELECT external_id FROM users)
  ON CONFLICT (email) DO NOTHING;

-- Step 4: Drop staging table (irreversible — data is now in users)
CREATE MIGRATION drop_import_staging NO ROLLBACK
DEPENDS ON import_users_from_staging
AS
  DROP TABLE import_users_staging;
```

Apply after loading the CSV:

```sql
APPLY MIGRATION create_import_staging;
-- (load CSV here)
APPLY MIGRATION import_users_from_staging;
APPLY MIGRATION drop_import_staging;
```
