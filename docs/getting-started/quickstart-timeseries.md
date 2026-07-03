# Quickstart: Time-Series

Ingest timestamped measurements and roll them up into time buckets. The
**time-series** model is a semantic layer over a `collection` (the universal
container): append-heavy points with a retention window and built-in
downsampling.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Declare the series

Set a 7-day retention window and a 1h->5m average downsample rule:

```sql
CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg;
```

## 3. Ingest points

Timestamps are nanoseconds; these span two 5-minute buckets:

```sql
INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{"host":"srv-a"}', 0);
INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{"host":"srv-a"}', 60000000000);
INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 30.0, '{"host":"srv-b"}', 300000000000);
```

## 4. Your first meaningful result

Bucket by five minutes and average each window:

```sql
SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m);
```

```text
 bucket | avg_value | samples
--------+-----------+--------
 0      | 15.0      | 2
 5m     | 30.0      | 1
```

## Where to go next

- [Time-Series](/data-models/timeseries.md) — the full time-series model
- [Hypertables](/data-models/hypertables.md) — partitioned time-series at scale
- [Continuous Aggregates](/data-models/continuous-aggregates.md) — always-fresh rollups
