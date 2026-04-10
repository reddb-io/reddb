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

```sql
-- Timestamp auto-generated if omitted
INSERT INTO cpu_metrics (metric, value, tags)
  VALUES ('cpu.idle', 95.2, '{"host":"srv1","region":"us-east"}')

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

### Range Query

```sql
SELECT metric, value, timestamp FROM cpu_metrics
  WHERE metric = 'cpu.idle'
    AND time BETWEEN '2024-01-01' AND '2024-01-02'
  ORDER BY time ASC
  LIMIT 1000
```

### Time-Bucket Aggregation

Group data into time windows with `time_bucket()`:

```sql
SELECT time_bucket(5m) AS bucket, avg(value), max(value), min(value)
  FROM cpu_metrics
  WHERE metric = 'cpu.idle'
    AND tags.host = 'srv1'
    AND time BETWEEN '2024-01-01' AND '2024-01-02'
  GROUP BY time_bucket(5m)
```

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

### Downsampled Query

```sql
SELECT downsample(value, '1h', 'avg') FROM cpu_metrics
  WHERE metric = 'cpu.idle'
```

### Tag Filtering

```sql
SELECT * FROM cpu_metrics
  WHERE metric = 'memory.used'
    AND tags.host IN ('srv1', 'srv2')
    AND time > now() - 1h
```

## Storage Architecture

Time-series data uses a chunked storage model for efficiency:

- **Points** are grouped by (metric, tags) into **chunks** of up to 1024 points
- **Delta-of-delta encoding** for timestamps: regular intervals compress to near-zero overhead
- **Gorilla XOR compression** for float values: similar consecutive values compress extremely well
- **Chunks seal automatically** when full, enabling immutable compressed storage
- **Retention policies** run in the maintenance cycle, dropping expired chunks

## Retention Policies

```sql
CREATE TIMESERIES sensor_data RETENTION 365 d
```

Duration units: `ms`, `s`, `m`, `h`, `d`

Data older than the retention period is automatically deleted during the maintenance cycle.

## See Also

- [INSERT](/query/insert.md) -- Inserting data
- [SELECT](/query/select.md) -- Querying data
- [Tables](/data-models/tables.md) -- Structured row storage
