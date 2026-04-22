# Logs Quickstart — 5 minutes to a working pipeline

This is the ultra-condensed version of
[Using RedDB for Logs](./using-reddb-for-logs.md). Copy, paste, read
the expanded guide when you want the why.

## 1. Declare the hypertable

```sql
CREATE HYPERTABLE logs (
  ts         BIGINT,
  service    TEXT    CODEC(Dict, LZ4),
  severity   INT     CODEC(T64),
  message    TEXT    CODEC(ZSTD(6)),
  trace_id   TEXT    CODEC(LZ4)
) CHUNK_INTERVAL '1d';

CREATE INDEX logs_service_ts ON logs (service, ts);
```

## 2. Ingest (batched)

```python
db.insert_many("logs", [
    {"ts": now_ns(), "service": "api",   "severity": 2, "message": "ok",    "trace_id": "t1"},
    {"ts": now_ns(), "service": "db",    "severity": 4, "message": "slow",  "trace_id": "t2"},
    {"ts": now_ns(), "service": "auth",  "severity": 3, "message": "retry", "trace_id": "t3"},
])
```

## 3. Dashboard query

```sql
SELECT time_bucket('1m', ts) AS bucket,
       service,
       count(*)                              AS hits,
       count_if(severity >= 4)               AS errors,
       quantileTDigest(0.99, latency_ms)     AS p99
FROM logs
WHERE ts >= NOW() - INTERVAL '1 hour'
GROUP BY bucket, service
ORDER BY bucket;
```

## 4. Continuous aggregate for fast dashboards

```sql
CREATE CONTINUOUS AGGREGATE logs_1m AS
SELECT time_bucket('1m', ts) bk, service,
       count(*) hits, count_if(severity >= 4) errors
FROM logs GROUP BY bk, service
WITH (refresh_lag = '30s');
```

Dashboards query `logs_1m` — sub-second response on billions of
rows.

## 5. Retention — pick one

**Declarative (simplest)**: attach a TTL at CREATE time. Chunks
disappear once their newest row passes the TTL — O(1) metadata
drop.

```sql
-- Either at creation:
CREATE HYPERTABLE logs (...) CHUNK_INTERVAL '1d' WITH (ttl = '30d');

-- Or after the fact via policy daemon:
SELECT add_retention_policy('logs', INTERVAL '30 days');
```

See [Partition TTL](../data-models/partition-ttl.md) for per-chunk
overrides, preview sweep, and the cost model.

## 6. Semantic search

```sql
CREATE EMBEDDING COLUMN message_vec ON logs (message)
  USING PROVIDER 'openai' MODEL 'text-embedding-3-small'
  ON CHANGE REFRESH;

SELECT ts, service, message
FROM logs
WHERE SIMILARITY(message_vec, EMBEDDING('database timeout', 'openai')) > 0.80
ORDER BY ts DESC
LIMIT 50;
```

## 7. Error anomaly classification (optional)

```sql
CREATE MODEL log_anomaly
  TYPE CLASSIFIER
  ALGORITHM LOGISTIC_REGRESSION
  FROM (SELECT message, (severity >= 4) AS is_error FROM logs LIMIT 100000)
  FEATURES (TF_IDF(message))
  TARGET is_error
  WITH (async = true);

SELECT ts, message, ML_CLASSIFY_PROBA('log_anomaly', message) AS anomaly_score
FROM logs
WHERE ts > NOW() - INTERVAL '10 minutes'
ORDER BY anomaly_score DESC
LIMIT 20;
```

## That's it

Read the [full guide](./using-reddb-for-logs.md) for schema design
best practices, comparison vs Loki / ClickHouse / Elasticsearch,
troubleshooting, and multi-model correlation patterns.
