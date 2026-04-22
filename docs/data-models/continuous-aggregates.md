# Continuous Aggregates

A continuous aggregate is an incrementally-materialised time-bucket
view over a hypertable / time-series collection. Queries hit the
pre-aggregated bucket map, not the raw rows. A background daemon
keeps the materialisation up to date by processing only buckets that
closed since the last refresh.

This is the core abstraction TimescaleDB pioneered as
`CREATE MATERIALIZED VIEW ... WITH (timescaledb.continuous = true)`.
RedDB's implementation is purpose-built for the time-axis:
incremental refresh is the default, not an add-on.

## Declaration

```sql
CREATE CONTINUOUS AGGREGATE five_min_load
AS SELECT
     time_bucket('5m', ts) AS bucket,
     avg(load)             AS avg_load,
     max(load)             AS max_load
   FROM metrics
   GROUP BY bucket
WITH (
  refresh_lag          = '1m',
  max_interval_per_job = '1d'
);
```

Parameters:

* `refresh_lag` — how far behind `now()` the refresh driver stays.
  Ensures we never materialise a bucket whose source rows are still
  landing. Default `0`.
* `max_interval_per_job` — hard cap on how much time a single
  refresh cycle processes. Stops an idle aggregate from locking a
  worker for hours when it finally runs. Default `+∞`.

## How refresh works

The engine tracks `last_refreshed_bucket` per aggregate. On each
cycle:

1. Compute the refresh window
   `[last_refreshed_bucket, now - refresh_lag)`, then cap by
   `max_interval_per_job`.
2. Ask the source table for rows whose timestamp falls in that
   window.
3. Fold each row into its bucket's in-memory state (`count`, `sum`,
   `min`, `max`, `first`, `last`).
4. Advance `last_refreshed_bucket` to the window's end.

Buckets outside the window are left alone — a refresh never re-reads
historical chunks.

## Supported aggregate functions

* `count(*)` / `count(col)`
* `sum(col)`
* `avg(col)`
* `min(col)` / `max(col)`
* `first(col)` / `last(col)` — first / last observed value in bucket.

`quantile` / `percentile` via T-Digest is tracked as follow-on work;
when it lands it reuses the same incremental machinery.

## Querying

```sql
SELECT bucket, avg_load
FROM five_min_load
WHERE bucket >= NOW() - INTERVAL '24h'
ORDER BY bucket;
```

The planner routes the query directly against the materialised
bucket map. For buckets not yet refreshed (inside the
`refresh_lag` window), queries fall back to scanning the source —
the view never lies about "fresh" rows.

## Dropping

```sql
DROP CONTINUOUS AGGREGATE five_min_load;
```

Removes the spec plus the materialised bucket map. Parent table is
untouched.

## Caveats (sprint scope)

* The SQL DDL surface lands in the sprint after B5 projections wire
  into the planner. The `ContinuousAggregateEngine` API is callable
  programmatically today — tests in
  `src/storage/timeseries/continuous_aggregate.rs` pin the
  incremental refresh arithmetic end-to-end.
* Refresh scheduling currently runs on the same background pool as
  the retention daemon. Dedicated per-aggregate schedule policies
  (hourly, cron-style) arrive once we split the pool.
