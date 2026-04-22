# Time-Series

RedDB includes a dedicated time-series data model optimized for high-volume, time-stamped metric data. No need for a separate InfluxDB or TimescaleDB.

## When to Use

- Server/application metrics (CPU, memory, latency)
- IoT sensor data
- Financial tick data
- Event logs with timestamps
- Any data where time is the primary query dimension

## Creating a Time-Series Collection

```sql
CREATE TIMESERIES cpu_metrics RETENTION 90 d
```

`CREATE TIMESERIES` is not just documentation. It persists the collection contract as a native
time-series model, and that is what makes `INSERT INTO cpu_metrics (...)` validate and store native
time-series points instead of generic table rows.

With downsampling policies:

```sql
CREATE TIMESERIES cpu_metrics
  RETENTION 90 d
  DOWNSAMPLE 1h:5m:avg, 1d:1h:avg
```

Parameters:

| Parameter | Description | Default |
|:----------|:------------|:--------|
| `RETENTION` | Auto-delete data older than duration | None (keep forever) |
| `DOWNSAMPLE` | `target:source:aggregation` policies | None |
| `CHUNK_SIZE` | Points per chunk before sealing | 1024 |

## Inserting Data Points

Supported SQL columns for native point inserts:

| Column | Required | Notes |
|:-------|:---------|:------|
| `metric` | Yes | Metric / series name, such as `cpu.idle` |
| `value` | Yes | Numeric sample value |
| `tags` | No | JSON object, either inline (`{host: 'srv1'}`) or JSON text |
| `timestamp` | No | Unix timestamp in nanoseconds |
| `timestamp_ns` | No | Same as `timestamp` |
| `time` | No | Alias for `timestamp` |

If you omit the timestamp column, RedDB assigns the current Unix time in nanoseconds.
Exactly one of `timestamp`, `timestamp_ns`, or `time` may be provided.

```sql
-- Timestamp auto-generated if omitted
INSERT INTO cpu_metrics (metric, value, tags)
  VALUES ('cpu.idle', 95.2, {host: 'srv1', region: 'us-east'})

-- Explicit timestamp (nanoseconds since epoch)
INSERT INTO cpu_metrics (metric, value, tags, timestamp)
  VALUES ('cpu.idle', 94.8, '{"host":"srv1"}', 1704067200000000000)
```

Bulk insert:

```sql
INSERT INTO cpu_metrics (metric, value, tags)
  VALUES ('cpu.idle', 95.2, '{"host":"srv1"}'),
         ('cpu.idle', 92.1, '{"host":"srv2"}'),
         ('mem.used', 72.5, '{"host":"srv1"}')
```

## Querying Time-Series Data

Native time-series records expose these query columns:

| Column | Meaning |
|:-------|:--------|
| `metric` | Metric / series name |
| `value` | Sample value |
| `timestamp_ns` | Native Unix timestamp in nanoseconds |
| `timestamp` | Alias for `timestamp_ns` |
| `time` | Alias for `timestamp_ns` |
| `tags` | JSON object with tag key/value pairs |

### Range Query

```sql
SELECT metric, value, timestamp FROM cpu_metrics
  WHERE metric = 'cpu.idle'
    AND timestamp BETWEEN 1704067200000000000 AND 1704153600000000000
  ORDER BY timestamp ASC
  LIMIT 1000
```

### Time-Bucket Aggregation

Group data into time windows with `time_bucket()`:

```sql
SELECT time_bucket(5m) AS bucket,
       avg(value) AS avg_value,
       max(value) AS max_value,
       min(value) AS min_value,
       count(*) AS samples
  FROM cpu_metrics
  WHERE metric = 'cpu.idle'
    AND timestamp BETWEEN 1704067200000000000 AND 1704153600000000000
  GROUP BY time_bucket(5m)
  ORDER BY bucket ASC
```

`time_bucket(5m)` uses the record timestamp automatically. If you need to point it at an explicit
timestamp column, use `time_bucket(5m, timestamp_ns)`.

Supported aggregation functions:

| Function | Description |
|:---------|:------------|
| `avg(value)` | Mean value in the bucket |
| `min(value)` | Minimum value |
| `max(value)` | Maximum value |
| `sum(value)` | Sum of values |
| `count(*)` | Number of data points |
| `first(value)` | First value in the bucket |
| `last(value)` | Last value in the bucket |

### Tag Filtering

```sql
SELECT * FROM cpu_metrics
  WHERE metric = 'memory.used'
    AND tags.host IN ('srv1', 'srv2')
    AND timestamp > 1704063600000000000
```

## Storage Architecture

Time-series data uses a chunked storage model for efficiency:

- **Points** are grouped by (metric, tags) into **chunks** of up to 1024 points
- **Delta-of-delta encoding** for timestamps: regular intervals compress to near-zero overhead
- **Gorilla XOR compression** for float values: similar consecutive values compress extremely well
- **Chunks seal automatically** when full, enabling immutable compressed storage
- **Retention policies** run in the maintenance cycle, dropping expired chunks

## Retention Policies

Three complementary knobs, strongest first:

```sql
-- (1) Partition TTL at CREATE time — declarative, O(1) chunk drop.
CREATE HYPERTABLE sensor_metrics (ts BIGINT, value DOUBLE)
  CHUNK_INTERVAL '1 day'
  WITH (ttl = '365d');

-- (2) Classic CREATE TIMESERIES RETENTION — single-collection shortcut.
CREATE TIMESERIES sensor_data RETENTION 365 d;

-- (3) Named retention policy on any time-bounded collection.
SELECT add_retention_policy('sensor_metrics', INTERVAL '365 days');
```

Duration units accepted everywhere: `ms`, `s`, `m`, `h`, `d`.

The retention daemon sweeps policies on a configurable interval
(`red.config.retention.interval_ms`) and drops expired chunks in
constant time per chunk — no row-level scans. See
[Partition TTL](./partition-ttl.md) for the full cost model and
mixed-TTL / per-chunk override recipes.

## Scaling out — hypertables + continuous aggregates

`CREATE TIMESERIES` stays the lightest surface for a single stream.
For workloads with many series, heavy analytics, or dashboards,
graduate to:

- [**Hypertables**](./hypertables.md) — auto chunking by time, O(1)
  `drop_chunks`, multi-column schemas, partition TTL.
- [**Continuous Aggregates**](./continuous-aggregates.md) —
  incrementally materialised `time_bucket` rollups; dashboards hit
  the rollup, not the raw chunks.

> [!TIP]
> If you skip `CREATE TIMESERIES` and insert into a brand-new collection directly, RedDB will
> auto-create a regular row collection. Use `CREATE TIMESERIES` whenever you want native point
> validation, retention metadata, and time-series query semantics.

## See Also

- [INSERT](/query/insert.md) -- Inserting data
- [SELECT](/query/select.md) -- Querying data
- [Tables](/data-models/tables.md) -- Structured row storage
- [Hypertables](/data-models/hypertables.md) -- Time-range partitioning
- [Continuous Aggregates](/data-models/continuous-aggregates.md) -- Incremental rollups
- [Partition TTL](/data-models/partition-ttl.md) -- Declarative chunk expiry
- [Using RedDB for Logs](/guides/using-reddb-for-logs.md) -- End-to-end log pipeline
