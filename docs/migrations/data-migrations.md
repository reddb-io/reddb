# Data Migrations

Schema migrations change structure. Data migrations change content — backfills,
normalizations, purges, denormalizations. Data migrations are where external
tools fall apart: they have no concept of a job that runs in chunks, persists
progress, and resumes safely after a crash.

RedDB's `BATCH N ROWS` clause and checkpoint system solve this directly in the
engine.

---

## What `BATCH N ROWS` does

When you declare `BATCH N ROWS`, RedDB does not execute the body as a single
statement. It rewrites the body into an iterative loop:

1. Execute the body with `LIMIT N` appended to the `WHERE` clause.
2. Count the affected rows. Persist `rows_processed += count` to
   `red_migrations`.
3. If the affected count equals `N`, there may be more rows. Go to step 1.
4. If the affected count is less than `N`, all matching rows have been
   processed. Mark the migration `applied`.

After each iteration, the checkpoint is committed to storage. If the process
is interrupted between iterations, the next `APPLY MIGRATION` call reads
`rows_processed`, skips already-processed rows, and continues from the
checkpoint.

### The loop body requirement

The body of a batched migration must be a single `UPDATE` or `DELETE`
statement with a `WHERE` clause that filters for unprocessed rows. RedDB
appends `LIMIT N` to that `WHERE` clause.

```sql
-- Correct: the WHERE clause identifies unprocessed rows
CREATE MIGRATION backfill_slug BATCH 2000 ROWS
AS
  UPDATE posts
  SET slug = lower(replace(title, ' ', '-'))
  WHERE slug IS NULL;
```

The `WHERE slug IS NULL` condition acts as the progress filter. Once a row
is processed (slug is set), it no longer matches the `WHERE` clause and is
skipped in subsequent batches.

### What RedDB appends internally

Given the example above with `BATCH 2000 ROWS`, each iteration executes:

```sql
UPDATE posts
SET slug = lower(replace(title, ' ', '-'))
WHERE slug IS NULL
LIMIT 2000;
```

You do not write the `LIMIT` yourself — RedDB manages it.

---

## Checkpoint resume

Checkpoints are the key property that makes `BATCH N ROWS` safe for
production data migrations on large tables.

### What is stored

After each batch, RedDB writes to `red_migrations`:

```sql
UPDATE red_migrations
SET rows_processed = rows_processed + <batch_affected_count>
WHERE name = '<migration_name>';
```

This is a durable write. If the process dies after the batch commits but
before the checkpoint write, the worst case is that the batch is re-applied
on resume (idempotent for the `WHERE slug IS NULL` pattern). If the process
dies after the checkpoint write, the next resume skips exactly those rows.

### Resuming after interruption

You do not need to do anything special to resume. Just re-run the apply:

```sql
APPLY MIGRATION backfill_slug;
```

RedDB reads `rows_processed` from `red_migrations`, determines that some
rows have already been processed, and continues. The migration is not
re-run from scratch.

If the migration was previously `status = 'failed'` due to an error in one
batch, you fix the root cause (e.g., a constraint violation on a specific
row), then re-run `APPLY MIGRATION`. The engine resumes from the checkpoint.

### Monitoring progress

While a long data migration runs, you can query progress from another session:

```sql
SELECT
  name,
  status,
  rows_processed,
  rows_total,
  round(rows_processed::float / nullif(rows_total, 0) * 100, 1) AS pct_complete
FROM red_migrations
WHERE name = 'backfill_slug';
```

```
 name          | status  | rows_processed | rows_total | pct_complete
---------------+---------+----------------+------------+--------------
 backfill_slug | applied | 840000         | 1200000    | 70.0
```

`rows_total` is the row count estimate from the query planner at the time
`APPLY MIGRATION` started. It is an estimate — the actual number of processed
rows may differ if rows are inserted or deleted during the migration.

---

## `NO ROLLBACK`

Some data migrations are intentionally one-way:

- You are deleting PII under a legal retention policy.
- You are purging rows that no longer have referential integrity.
- You are overwriting source data with a normalized value and the original
  cannot be reconstructed.

For these cases, declare `NO ROLLBACK`:

```sql
CREATE MIGRATION purge_deleted_accounts BATCH 500 ROWS NO ROLLBACK
AS
  DELETE FROM users WHERE deleted_at < now() - INTERVAL '90 days';
```

Attempting to roll back this migration returns:

```
ERROR: migration 'purge_deleted_accounts' is marked NO ROLLBACK and cannot be reverted
```

### When to use `NO ROLLBACK`

Use `NO ROLLBACK` when:

| Situation | Reason |
|---|---|
| `DROP COLUMN` | The column data is permanently discarded at the storage level. |
| Overwrite backfill | The original values are gone; VCS revert would restore logically invalid data. |
| PII purge | Legal or compliance requirement — you must not be able to restore the data. |
| Truncation | Reverting a `TRUNCATE` via VCS would restore data you explicitly chose to delete. |

Do not use `NO ROLLBACK` for:

| Situation | Reason |
|---|---|
| Adding a column | Adding a column is always reversible via `DROP COLUMN`. |
| Normalizing into a new column | If the source column still exists, rollback is safe. |
| `UPDATE` that sets a nullable column | The original nulls are preserved in VCS; rollback is safe. |

### `NO ROLLBACK` and VCS

When a `NO ROLLBACK` migration is applied, a VCS commit is still created and
`vcs_commit_hash` is still set. The commit exists in the history. The engine
simply refuses to call `vcs_revert` on it via `ROLLBACK MIGRATION`.

You can still inspect the commit, diff against it, and perform time-travel
queries against the state immediately before the migration ran:

```sql
SELECT count(*) FROM users
AS OF COMMIT 'commit-hash-before-purge';
```

This is a read-only operation — it does not restore data.

---

## Choosing a batch size

Batch size is a tradeoff between throughput, lock contention, and checkpoint
granularity.

| Batch size | Lock duration | Checkpoint granularity | Good for |
|---|---|---|---|
| 100–500 | Very short | Very fine | Tables with heavy concurrent writes; critical production systems |
| 1,000–5,000 | Short | Fine | Standard production backfills |
| 10,000–50,000 | Medium | Coarse | Large tables with low concurrent write pressure |
| 100,000+ | Long | Very coarse | Offline or maintenance-window migrations |

**Guidelines:**

- Start with 1,000 rows and measure lock wait time. Increase until you see
  contention.
- Prefer smaller batches if the table receives heavy writes during the migration
  — shorter lock windows reduce the chance of blocking application queries.
- For pure `DELETE` purges on append-only or low-write tables, 10,000–50,000
  rows per batch is usually fine.
- Never use `BATCH 1 ROW` — the per-row overhead of the checkpoint write
  dominates.

---

## Multi-statement bodies in data migrations

`BATCH N ROWS` applies to the first (and typically only) DML statement in
the body. If you need to update multiple tables in a coordinated migration,
write separate migrations with explicit `DEPENDS ON`:

```sql
-- Do this: two separate migrations with a dependency
CREATE MIGRATION backfill_order_totals BATCH 5000 ROWS
AS
  UPDATE orders
  SET total_cents = subtotal_cents + tax_cents
  WHERE total_cents IS NULL;

CREATE MIGRATION backfill_invoice_totals BATCH 5000 ROWS
DEPENDS ON backfill_order_totals
AS
  UPDATE invoices
  SET total_cents = (
    SELECT sum(total_cents) FROM orders WHERE invoice_id = invoices.id
  )
  WHERE total_cents IS NULL;
```

Keeping them separate gives you independent progress tracking, independent
rollback, and independent resume on failure.

---

## Example: full lifecycle of a large data migration

```sql
-- 1. Register
CREATE MIGRATION normalize_phone_numbers BATCH 2000 ROWS
AS
  UPDATE contacts
  SET phone = regexp_replace(phone, '[^0-9+]', '', 'g')
  WHERE phone != regexp_replace(phone, '[^0-9+]', '', 'g');

-- 2. Inspect before applying
EXPLAIN MIGRATION normalize_phone_numbers;

-- 3. Apply (will loop in batches of 2000)
APPLY MIGRATION normalize_phone_numbers;

-- 4. Monitor from another session while running
SELECT name, rows_processed, rows_total
FROM red_migrations
WHERE name = 'normalize_phone_numbers';

-- 5. If interrupted, just re-run — checkpoint resumes automatically
APPLY MIGRATION normalize_phone_numbers;

-- 6. After completion, verify
SELECT count(*) FROM contacts
WHERE phone != regexp_replace(phone, '[^0-9+]', '', 'g');
-- Expected: 0
```
