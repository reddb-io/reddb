# Views & Materialized Views

RedDB supports both virtual views (query rewriting) and materialized
views (cached results with refresh policies).

## Regular views

Stored SQL that executes on every reference:

```sql
CREATE VIEW active_users AS
  SELECT id, email, last_seen
  FROM users
  WHERE status = 'active';

-- use as any table
SELECT count(*) FROM active_users;
SELECT * FROM active_users WHERE last_seen > '2026-01-01';
```

`CREATE OR REPLACE VIEW` updates an existing definition:

```sql
CREATE OR REPLACE VIEW active_users AS
  SELECT id, email, last_seen, tenant_id
  FROM users
  WHERE status = 'active' AND deleted_at IS NULL;
```

Drop:

```sql
DROP VIEW [IF EXISTS] active_users;
```

### Nested views

The rewriter descends `TableSource::Subquery` recursively, so views
that reference other views work:

```sql
CREATE VIEW acme_users AS
  SELECT * FROM active_users WHERE tenant_id = 'acme';

SELECT count(*) FROM acme_users;
-- rewrites to: SELECT count(*) FROM (SELECT ... FROM (SELECT ... FROM users ...))
```

## Materialized views

Cache query results, refresh on demand:

```sql
CREATE MATERIALIZED VIEW user_stats AS
  SELECT
    tenant_id,
    count(*) AS total,
    count(*) FILTER (WHERE status = 'active') AS active_count,
    max(last_seen) AS last_activity
  FROM users
  GROUP BY tenant_id;

-- read the cache (fast)
SELECT * FROM user_stats;

-- manual refresh
REFRESH MATERIALIZED VIEW user_stats;
```

### Refresh policies

Materialized views track a refresh policy in the cache layer
(`MaterializedViewCache`). Three modes:

| Policy | Trigger |
|--------|---------|
| **Manual** (default) | Only `REFRESH MATERIALIZED VIEW` |
| **On-write** | Auto-refresh when any source table is written |
| **Interval** | Auto-refresh every N seconds |

```sql
CREATE MATERIALIZED VIEW user_stats
  WITH REFRESH POLICY on_write
  AS
  SELECT tenant_id, count(*) FROM users GROUP BY tenant_id;

CREATE MATERIALIZED VIEW hourly_activity
  WITH REFRESH POLICY every 3600 seconds
  AS
  SELECT date_trunc('hour', ts) AS bucket, count(*)
  FROM events
  GROUP BY bucket;
```

## Views with RLS and tenancy

Views inherit the RLS policies of their source tables at evaluation
time. A view over a tenant-scoped table is automatically
tenant-scoped:

```sql
CREATE TABLE orders (...) TENANT BY (org);

CREATE VIEW recent_orders AS
  SELECT * FROM orders WHERE placed_at > now() - interval '7 days';

SET TENANT 'acme';
SELECT * FROM recent_orders;   -- only 'acme' orders
```

## When to use which

| Need | Choose |
|------|--------|
| Logical rename / projection of columns | Regular view |
| Hide columns from clients | Regular view |
| Precompute an expensive aggregate | Materialized view |
| Refresh cached dashboards at fixed intervals | Materialized view + interval policy |
| Always-fresh reads | Regular view |
| Results must survive source-table deletes | Materialized view |

## Limitations

- View definitions live in memory — persist across restart is on the
  roadmap (materialized-view cache already writes to `red_config`).
- No indexable materialized views yet (read-only cache).
- `WITH (security_barrier = true)` PG option not yet honoured —
  predicates are always pushed through the rewrite.

## See also

- [SELECT](select.md)
- [Row Level Security](../security/rls.md)
