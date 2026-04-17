# Partitioning

RedDB supports three PostgreSQL partition strategies: RANGE, LIST,
and HASH. Partitioning splits a parent table into child collections
bound by a key predicate — useful for large tables where retention,
pruning, or parallel scans matter.

## Declaring a partitioned parent

```sql
CREATE TABLE events (
  id BIGINT,
  ts TIMESTAMP,
  payload JSON
) PARTITION BY RANGE (ts);
```

The parent holds no rows itself — it's a routing / registry entry.
Actual data lives in child partitions bound via `ATTACH PARTITION`.

## Partition strategies

### Range

Good for time-series where each partition covers a date window.

```sql
CREATE TABLE events (id BIGINT, ts TIMESTAMP, payload JSON)
  PARTITION BY RANGE (ts);

CREATE TABLE events_2025_q4 (id BIGINT, ts TIMESTAMP, payload JSON);
CREATE TABLE events_2026_q1 (id BIGINT, ts TIMESTAMP, payload JSON);

ALTER TABLE events ATTACH PARTITION events_2025_q4
  FOR VALUES FROM ('2025-10-01') TO ('2026-01-01');

ALTER TABLE events ATTACH PARTITION events_2026_q1
  FOR VALUES FROM ('2026-01-01') TO ('2026-04-01');
```

### List

Good for fixed enumerations like region, status, or product category.

```sql
CREATE TABLE orders (id BIGINT, region TEXT, total DECIMAL)
  PARTITION BY LIST (region);

ALTER TABLE orders ATTACH PARTITION orders_americas
  FOR VALUES IN ('us', 'ca', 'br', 'mx');

ALTER TABLE orders ATTACH PARTITION orders_europe
  FOR VALUES IN ('de', 'fr', 'uk', 'es');
```

### Hash

Good for even row distribution when there's no natural range/list
discriminator.

```sql
CREATE TABLE metrics (id BIGINT, host TEXT, value DOUBLE)
  PARTITION BY HASH (host);

ALTER TABLE metrics ATTACH PARTITION metrics_p0
  FOR VALUES WITH (MODULUS 4, REMAINDER 0);

ALTER TABLE metrics ATTACH PARTITION metrics_p1
  FOR VALUES WITH (MODULUS 4, REMAINDER 1);

ALTER TABLE metrics ATTACH PARTITION metrics_p2
  FOR VALUES WITH (MODULUS 4, REMAINDER 2);

ALTER TABLE metrics ATTACH PARTITION metrics_p3
  FOR VALUES WITH (MODULUS 4, REMAINDER 3);
```

## Detaching

```sql
-- stop routing new rows, keep the child table
ALTER TABLE events DETACH PARTITION events_2025_q4;

-- now safe to archive or drop
DROP TABLE events_2025_q4;
```

## Registry storage

Partition metadata is persisted in `red_config`:

- `partition.{parent}.by` — kind (`range` / `list` / `hash`)
- `partition.{parent}.column` — partition key
- Child bounds are round-tripped as raw strings under the same prefix

This means partition topology survives restart without any extra
rehydration logic.

## Current status

Phase 2.2 (registry-only):
- Parser + AST: ✅
- Parent declaration + attach/detach: ✅
- Catalog persistence: ✅
- **Partition pruning in the planner: Phase 4 — not yet live**

Today, reads against the parent don't automatically fan out into
children. Applications that need the pruning benefit should route
queries at the application layer by date / region / hash, or wait
for Phase 4's planner rewrite.

## Recipe: rolling retention window

Drop old quarters by detaching the partition — constant-time
regardless of row count:

```sql
ALTER TABLE events DETACH PARTITION events_2025_q1;
DROP TABLE events_2025_q1;
```

No tombstones, no VACUUM. The next `ATTACH PARTITION` claims a new
range for fresh data.

## See also

- [CREATE TABLE](create-table.md)
- [Maintenance (VACUUM / ANALYZE)](maintenance.md)
