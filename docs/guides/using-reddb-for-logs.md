# Using RedDB for Logs

> **TL;DR** — RedDB is a serious log store if you want logs + metrics +
> traces + semantic search in **one SQL-native engine**. This guide
> walks you from schema design → ingest → dashboards → retention →
> multi-model correlation, with honest notes on where we win vs
> Loki / Elasticsearch / ClickHouse and where we don't yet.

---

## Why RedDB for logs?

Most stacks glue together:

| Need                          | Typical system |
|-------------------------------|---------------|
| Raw ingest                    | Loki / Fluentd / Vector |
| SQL analytics on logs         | ClickHouse / BigQuery |
| Full-text / fuzzy search      | Elasticsearch / OpenSearch |
| Metrics + dashboards          | Prometheus + Grafana |
| Traces                        | Jaeger / Tempo |
| Anomaly / classification      | Python pipelines |

RedDB collapses those into one engine with **one SQL surface**:

* Append-only tables + hypertables → ingest path
* Per-column codecs (Dict / LZ4 / ZSTD / Delta / DoubleDelta) → 5–20×
  compression on typical log shapes
* Continuous aggregates → dashboards that don't rescan
* `quantileTDigest` / `uniq` / `topK` / `count_if` → p99, unique
  users, top endpoints
* Vector + hybrid search → semantic log search (`"logs parecidos
  com este incidente"`)
* Graph store → trace / span traversal
* Retention daemon → O(1) chunk drops
* ML classifier + semantic cache → anomaly / classification
  over logs without leaving the DB

You don't have to use everything — start with tables + codecs + MVs;
the rest is available when you need it.

---

## 1. Pick the right model

RedDB has three append-flavoured models; pick based on shape:

| If the log is…                                      | Use |
|-----------------------------------------------------|-----|
| Free-form JSON / text, millions of lines/s, no SQL  | **Log Collections** (`POST /logs/{name}/append`) |
| Typed rows (status, latency, service, user_id)      | **`CREATE TABLE ... APPEND ONLY`** |
| Has a dominant time axis + need time_bucket / retention / drop_chunks | **`CREATE HYPERTABLE`** |

**Rule of thumb**: use **hypertables** for anything you'd put in
Prometheus / Timescale / Influx — that's 80% of log workloads. Use
**log collections** only when you can't enforce a schema at write time.

---

## 2. A realistic schema

Let's model an HTTP access log with traces:

```sql
CREATE HYPERTABLE access_log (
  ts          BIGINT           CODEC(DoubleDelta, ZSTD(3)),
  service     TEXT             CODEC(Dict, LZ4),
  severity    INT              CODEC(T64),
  status      INT              CODEC(T64),
  method      TEXT             CODEC(Dict),
  path        TEXT             CODEC(Dict, LZ4),
  latency_ms  INT              CODEC(Delta),
  bytes_out   BIGINT           CODEC(Delta, ZSTD(3)),
  user_id     BIGINT           CODEC(Delta),
  region      TEXT             CODEC(Dict),
  client_ip   TEXT             CODEC(Dict, LZ4),
  trace_id    TEXT             CODEC(LZ4),
  span_id     TEXT             CODEC(LZ4),
  message     TEXT             CODEC(ZSTD(6))
) CHUNK_INTERVAL '1d';
```

Codec choice rationale:

* **`Dict`** on `service` / `method` / `region` / `path` — cardinality
  is tiny compared to the row count. Expect 20–50× reduction.
* **`T64`** on `status` / `severity` — values fit in 10 bits, dictionary
  overhead isn't worth it.
* **`Delta`** / **`DoubleDelta`** on `ts`, `latency_ms`, `bytes_out`,
  `user_id` — typically monotonic or slowly drifting.
* **`ZSTD`** on `message` — high-entropy free text; still wins 2–3×.
* **`LZ4`** on `trace_id` / `span_id` — UUID-like strings with
  byte-level repetition only.

> **Landing status**: the `CODEC(...)` DDL syntax is parsed by the
> B3 codec pipeline but the parser bridge lands in the next sprint.
> Until then declare the columns without `CODEC(...)` and call
> [`encode_bytes`](../engine/columnar-execution.md#codecs) from the
> ingest path.

Indexes worth declaring now:

```sql
CREATE INDEX access_log_service_ts  ON access_log (service, ts);
CREATE INDEX access_log_status_hash ON access_log USING HASH (status);
CREATE INDEX access_log_trace_id    ON access_log (trace_id);
```

---

## 3. Ingest

### HTTP — JSON-first (recommended)

Don't synthesise INSERT statements from your log pipeline. Use the
`/ingest/{collection}` endpoint with a JSON body:

```bash
curl -X POST http://localhost:8080/ingest/access_log \
  -H 'Content-Type: application/json' \
  -d '[
    {"ts": 1714000000000000000, "service": "api", "severity": 2, "status": 200, "method": "GET", "path": "/health", "latency_ms": 3},
    {"ts": 1714000000100000000, "service": "api", "severity": 3, "status": 500, "method": "POST", "path": "/checkout", "latency_ms": 842}
  ]'
```

Or pipe NDJSON from any tool that can emit one JSON object per line:

```bash
cat today.ndjson | curl -X POST http://localhost:8080/ingest/access_log \
  -H 'Content-Type: application/x-ndjson' --data-binary @-
```

See the [Ingest API](../api/ingest.md) page for the full contract
(ack shape, envelope form, WebSocket, Vector / Fluent Bit config,
auth, troubleshooting).

### HTTP — SQL (when you need expressions)

```bash
curl -X POST http://localhost:8080/sql \
  -H 'Content-Type: application/json' \
  -d '{"query":"INSERT INTO access_log (ts, service, severity, status, method, path, latency_ms) VALUES (1714000000000000000, '\''api'\'', 2, 200, '\''GET'\'', '\''/health'\'', 3)"}'
```

### gRPC / drivers

Python:

```python
from reddb import Client

db = Client("localhost:50051")
db.insert_many("access_log", [
    {"ts": 1_714_000_000_000_000_000, "service": "api", "severity": 2,
     "status": 200, "method": "GET", "path": "/health",
     "latency_ms": 3},
    ...  # hundreds per batch for best throughput
])
```

Node / Bun:

```js
import { Client } from "@reddb/client";
const db = new Client("localhost:50051");
await db.insertMany("access_log", batch);
```

Rust (in-process, highest throughput):

```rust
use reddb::storage::timeseries::{LogLine, LogPipeline, LogSeverity};

let pipe = LogPipeline::new("access_log", "ts", "1d").unwrap();
pipe.set_retention(90 * 86_400); // 90 days

let lines: Vec<LogLine> = events.into_iter().map(|e| {
    LogLine::now(LogSeverity::Info, &e.service, &e.message)
        .with_field("latency_ms", e.latency_ms)
        .with_field("status", e.status as f64)
        .with_label("path", &e.path)
        .with_label("region", &e.region)
        .with_trace(&e.trace_id, &e.span_id)
}).collect();

pipe.ingest_batch(&lines);
```

### Performance tips

* **Always batch**. Hundreds of rows per INSERT. Single-row inserts
  hit ~30k/s; batched inserts hit ~800k/s on a laptop.
* **Keep `ts` monotonic per writer** — it lets Delta codec win
  maximum ratio on timestamps.
* Pre-group by `service`: your writer thread should emit contiguous
  runs of the same service so the `Dict` codec compresses best.
* Use the `application` Rust port (`LogPipeline::ingest_batch`) when
  you're in-process — it bypasses the wire layer entirely.

---

## 4. Querying

### Point lookups

```sql
-- A single trace's spans, chronologically
SELECT ts, service, span_id, latency_ms, message
FROM access_log
WHERE trace_id = 'c4fc28e8-9a1e-4a7e-9a5f-e4c3b8f2a1d9'
ORDER BY ts;
```

Partition pruning drops every chunk whose time range can't contain
rows for that trace.

### Time-series dashboards

```sql
SELECT time_bucket('1m', ts)                  AS bucket,
       service,
       count(*)                                AS hits,
       count_if(status >= 500)                 AS errors_5xx,
       quantileTDigest(0.50, latency_ms)       AS p50,
       quantileTDigest(0.99, latency_ms)       AS p99,
       uniq(user_id)                           AS unique_users,
       sum(bytes_out)                          AS bytes_served
FROM access_log
WHERE ts >= NOW() - INTERVAL '24 hours'
GROUP BY bucket, service
ORDER BY bucket DESC;
```

Every aggregate in that query is native: `count_if`, `quantileTDigest`,
`uniq`, `sum` — you don't need a separate analytics engine.

### Continuous aggregates (pre-materialised)

```sql
CREATE CONTINUOUS AGGREGATE access_5m AS
SELECT time_bucket('5m', ts)              AS bucket,
       service,
       count(*)                            AS hits,
       count_if(status >= 500)             AS errors,
       avg(latency_ms)                     AS avg_lat,
       quantileTDigest(0.99, latency_ms)   AS p99
FROM access_log
GROUP BY bucket, service
WITH (refresh_lag = '30s', max_interval_per_job = '1h');
```

Dashboards hit `access_5m` — instant, constant-time response even on
billions of rows. The refresh daemon fills new buckets every
refresh cycle; **you never hand-schedule anything**.

### Error ranking ("what's blowing up")

```sql
SELECT path, count(*) AS bad, topK(5, client_ip) AS top_offenders
FROM access_log
WHERE ts >= NOW() - INTERVAL '1 hour'
  AND status >= 500
GROUP BY path
ORDER BY bad DESC
LIMIT 10;
```

### Tail (`tail -f`-style)

```sql
-- Poll-based (every dashboard does this)
SELECT * FROM access_log
WHERE ts > $last_ts_watermark
ORDER BY ts
LIMIT 500;
```

In-process Rust callers can also use the bounded ring buffer:

```rust
let new_lines = pipe.tail_since(last_watermark_ns);
```

It's back-pressured by `recent_capacity` (default 4096) so a slow
consumer can't grow unbounded.

### Semantic search ("logs like this one")

```sql
SELECT id, ts, message,
       SIMILARITY(message_vec, EMBEDDING('payment refused', 'openai')) AS score
FROM access_log
WHERE ts >= NOW() - INTERVAL '7 days'
  AND SIMILARITY(message_vec, EMBEDDING('payment refused', 'openai')) > 0.75
ORDER BY score DESC
LIMIT 20
RERANK WITH (provider = 'cohere', model = 'rerank-english-v3.0');
```

Embedding columns keep themselves in sync via the managed lifecycle
(CDC-backed refresh). Re-ranking is declarative; the planner
batches + caches calls.

---

## 5. Retention

Four knobs — pick the one that matches your needs; they stack
cleanly.

### Partition TTL (declarative, fastest)

The simplest form: declare the TTL in the DDL itself, and RedDB
drops whole chunks once their newest row is older than the TTL.
O(1) per chunk — no row scans, no DELETE tombstones.

```sql
CREATE HYPERTABLE logs (...) CHUNK_INTERVAL '1d' WITH (ttl = '90d');
```

See [Partition TTL](../data-models/partition-ttl.md) for the full
semantics including per-chunk overrides (mixed-TTL hypertables),
preview sweep, and comparison against TimescaleDB / ClickHouse.

### Chunk-based drop (fastest, O(1))

```sql
SELECT drop_chunks('access_log', INTERVAL '90 days');
```

Chunks whose `max_ts` is older than 90 days disappear — no row scan.

### Automatic retention policy

```sql
SELECT add_retention_policy('access_log', INTERVAL '90 days');
```

The retention daemon scans policies every 60s (configurable via
`red.config.retention.interval_ms`) and drops chunks as they age.
Stats are queryable:

```sql
SELECT * FROM ML_JOBS WHERE kind = 'retention' ORDER BY started_at DESC LIMIT 10;
```

### Tiered — hot to cold

```sql
-- Downsample raw-resolution data after 7 days into a 1-minute rollup
CREATE CONTINUOUS AGGREGATE access_1m AS
SELECT time_bucket('1m', ts) bk, service, count(*) c, ...
FROM access_log GROUP BY bk, service;

SELECT add_retention_policy('access_log', INTERVAL '7 days');
-- access_1m has no retention — it's your long-term history.
```

7 days of raw detail + infinite 1-minute rollup. Storage flatlines
at whatever the rollup weighs, irrespective of ingest rate.

---

## 6. Correlation across models — the differentiator

A classic incident question: *"For this slow endpoint, which users
saw errors and what did they click next?"*

```sql
WITH slow_requests AS (
  SELECT trace_id, user_id, ts
  FROM access_log
  WHERE path = '/checkout'
    AND latency_ms > 2000
    AND status >= 500
    AND ts >= NOW() - INTERVAL '1 hour'
)
SELECT s.user_id,
       s.ts                                  AS error_ts,
       next_click.path                       AS next_path,
       next_click.latency_ms                 AS next_latency,
       FOLLOW(user_graph, s.user_id, 'same_device', UP TO 1) AS related_users
FROM slow_requests s
JOIN LATERAL (
  SELECT path, ts, latency_ms
  FROM access_log
  WHERE user_id = s.user_id
    AND ts > s.ts
  ORDER BY ts
  LIMIT 1
) AS next_click ON true;
```

* SQL table (`access_log`) → facts
* Graph (`user_graph`) → related users via `same_device` edges
* LATERAL join → "next row per user"

Try expressing that across Loki + Elasticsearch + Neo4j + your
in-house Python script.

---

## 7. Dashboards: example Grafana panel

Point Grafana at the SQL endpoint:

```yaml
datasources:
  - name: RedDB
    type: postgres           # PG-wire compat
    url: localhost:5433
    database: prod
```

Panel query:

```sql
SELECT bucket AS time,
       service,
       hits
FROM access_5m
WHERE bucket > $__timeFrom()
ORDER BY bucket;
```

Time-series panel renders instantly from the continuous aggregate.

---

## 8. Observability: watching RedDB itself

```sql
-- Ingest rate
SELECT count(*) AS lines_last_min
FROM access_log
WHERE ts > (NOW() - INTERVAL '1 minute');

-- Chunk count + size
SELECT * FROM show_chunks('access_log');

-- Retention activity
SELECT * FROM ML_JOBS WHERE kind = 'retention' ORDER BY started_at DESC LIMIT 5;

-- Continuous aggregate freshness
SELECT name, last_refreshed_bucket
FROM ML_MODELS_DASHBOARD   -- reused view for all incremental engines
WHERE name LIKE '%access%';
```

---

## 9. Comparison

### vs Loki

| Dim                    | Loki | RedDB |
|------------------------|------|-------|
| Write throughput       | ⭐⭐⭐⭐ | ⭐⭐⭐ |
| Compression ratio      | ⭐⭐⭐ | ⭐⭐⭐⭐ (per-column codecs) |
| SQL analytics          | ❌ (LogQL only) | ✅ |
| Full-text search       | ⭐⭐ (greps) | ⭐⭐ (BM25) + ⭐⭐⭐⭐ semantic |
| Traces / graph         | ❌ | ✅ |
| Multi-model (logs + metrics + docs + vectors) | ❌ | ✅ |

### vs ClickHouse

| Dim                    | ClickHouse | RedDB |
|------------------------|-----------|-------|
| OLAP on billions of rows | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ (gap documented) |
| Compression codecs     | ⭐⭐⭐⭐⭐ (same surface)| ⭐⭐⭐⭐ (same codec family) |
| Time bucket / MVs      | ⭐⭐⭐⭐ (MVs, some effort) | ⭐⭐⭐⭐⭐ (continuous aggregates native) |
| Vector / graph / ML    | ❌ (via UDFs) | ✅ native |
| Operational complexity | ⭐⭐ (cluster setup hard) | ⭐⭐⭐⭐ (single binary) |

### vs Elasticsearch

| Dim                    | Elasticsearch | RedDB |
|------------------------|--------------|-------|
| Full-text search       | ⭐⭐⭐⭐⭐ | ⭐⭐ BM25 + ⭐⭐⭐⭐ semantic |
| Typed SQL analytics    | ⭐⭐ (SQL-ish) | ⭐⭐⭐⭐⭐ |
| Storage footprint      | ⭐⭐ (heavy) | ⭐⭐⭐⭐ (codec-dense) |
| JVM dependency         | yes | no — single Rust binary |
| ML classifier inline   | plugin | ✅ native |

### Short version

Use RedDB when:

* you want to keep logs, metrics, traces, and semantic search in
  one engine,
* you want SQL-first analytics,
* you can budget roughly the same ingest rate Loki delivers but
  want richer queries and ML on top.

**Don't** use RedDB when:

* you need multi-TB/day distributed ingest right now (wait for the
  distributed roadmap — see [`distributed-roadmap.md`](../architecture/distributed-roadmap.md)),
* your primary workload is fuzzy text search on enormous corpora
  (Elasticsearch is still the sharper tool there today).

---

## 10. Troubleshooting

**Q: Ingest rate is low.**
Check `red.config.wal.group_commit = true` (it is by default). Batch
your writes — 100+ rows per INSERT is the target. Run the ingest on
the same process as the store when possible (`LogPipeline`).

**Q: `time_bucket` queries scan every chunk.**
Check that the `WHERE` filter uses the partition key column
directly (`WHERE ts >= ... AND ts < ...`). Function-wrapped
predicates (`WHERE date_trunc('hour', ts) = ...`) don't prune.

**Q: Continuous aggregate lags behind.**
Inspect `refresh_lag` — the daemon deliberately stays that far behind
`NOW()` to let in-flight writes settle. Shrink it (min: 1 s).

**Q: Disk usage keeps climbing.**
`add_retention_policy` OR `drop_chunks` OR set up a downsample +
retention pair (§ 5). Each chunk is independent; drop is instant.

**Q: I need `DELETE FROM log WHERE user_id = ...` for GDPR.**
Append-only blocks ad-hoc DELETEs. The GDPR-compatible pattern is:
keep a separate "redactions" table keyed by `user_id`, and
left-anti-join at query time. When the retention window expires,
the data disappears with the chunk. If you need active erase,
declare the table without `APPEND ONLY` and accept the UPDATE cost.

---

## 11. Reference

Core primitives this guide uses:

* [Hypertables](../data-models/hypertables.md) — chunking, drop_chunks.
* [Continuous aggregates](../data-models/continuous-aggregates.md) — incremental MVs.
* [Append-only tables](../data-models/append-only-tables.md) — declarative immutability.
* [Columnar execution + codecs](../engine/columnar-execution.md) — compression.
* [Competitive positioning](../architecture/competitive-positioning.md) — full TS/CH comparison.
* `reddb::storage::timeseries::LogPipeline` — the Rust helper shown in §3.

Questions / gaps / benchmarks to add? Open an issue tagged `logs`.
