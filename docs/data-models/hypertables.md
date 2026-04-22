# Hypertables

Hypertables are time-range-partitioned tables. A single logical
collection automatically splits writes into child chunks covering a
fixed time interval; queries that filter by the time column see the
partition pruner eliminate chunks that cannot match; operators can
drop entire chunks to enforce retention without a row-level scan.

This is the same model TimescaleDB popularised. RedDB's
implementation reuses the existing `TimeSeriesChunk` physical layer
(Delta-of-Delta timestamps + XOR values + T64 integer bit-packing +
optional zstd) — the hypertable layer sits on top as a router and
catalog.

## Mental model

A hypertable is the logical name you query. Behind it, writes fan
out into fixed-width child chunks:

```text
  CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d';

  INSERT ts=…-03-01T00 → chunk metrics:2026-03-01
  INSERT ts=…-03-01T05 → chunk metrics:2026-03-01
  INSERT ts=…-03-02T01 → chunk metrics:2026-03-02   ← auto-allocated
  INSERT ts=…-03-02T23 → chunk metrics:2026-03-02
  INSERT ts=…-03-04T10 → chunk metrics:2026-03-04   ← auto-allocated

  SELECT … WHERE ts BETWEEN 2026-03-02 AND 2026-03-02T23
    → planner prunes to {metrics:2026-03-02}, skips the other two
```

The chunk width is the tuning knob. Too small and you pay bookkeeping
overhead per chunk; too large and pruning stops giving you wins.
Start with the window your dashboard queries typically cover (one day
for operational metrics, one hour for high-cardinality telemetry) and
measure. `CHUNK_INTERVAL` can be changed for new chunks later, but
existing chunks keep their original width.

## Declaration

```sql
-- Shipped today — minimal hypertable with chunk routing + TTL.
CREATE HYPERTABLE metrics
  TIME_COLUMN ts
  CHUNK_INTERVAL '1d'
  TTL '90d';
```

`TIME_COLUMN` names the column carrying the nanosecond timestamp
axis. `CHUNK_INTERVAL` accepts any duration the retention grammar
understands (`30s`, `5m`, `1h`, `1d`, …). `TTL` is optional and
installs a default retention per chunk — the sweep drops chunks once
`max_ts + ttl < now`.

### Introspection and pruning

```sql
-- List every registered hypertable.
SELECT LIST_HYPERTABLES();
-- => ['metrics', 'events']

-- Ask the pruner which chunks overlap a window — exactly what the
-- planner consults before a scan.
SELECT HYPERTABLE_PRUNE_CHUNKS('metrics', lo_ns, hi_ns);
-- => ['metrics:1700000000000000000']  -- one chunk in the window

-- Remove the hypertable (clears the registry + drops the backing
-- collection). Registered continuous aggregates on top are dropped
-- separately.
DROP HYPERTABLE metrics;
```

### Extended column syntax (planned)

```sql
CREATE HYPERTABLE metrics (
  ts    BIGINT,
  host  TEXT,
  value DOUBLE
) CHUNK_INTERVAL '1d';
```

Column lists alongside the DDL land with the unified
`CREATE TABLE ... PARTITION BY TIME` rewrite in a follow-on sprint.
Until then declare schemas via `CREATE TABLE` + `CREATE HYPERTABLE`
pointing at the same name, or rely on dynamic typing.

## API

| Statement                                     | Behaviour |
|-----------------------------------------------|-----------|
| `INSERT INTO metrics VALUES (...)`           | Router sends the row to the chunk whose `[start, start+interval)` contains `ts`. Chunks are allocated on first write. |
| `SELECT ... FROM metrics WHERE ts >= X AND ts < Y` | Planner invokes the partition pruner to skip chunks whose bounds fall outside `[X, Y)`. |
| `SELECT show_chunks('metrics')`               | Lists every chunk with its bounds and row count. |
| `SELECT drop_chunks('metrics', INTERVAL '90 days')` | Drops every chunk whose max timestamp is older than `now() - 90 days`. |
| `DROP TABLE metrics`                          | Removes the spec and every child chunk. |

## Retention + Continuous Aggregates

Hypertables integrate with:

* [Retention policies](./retention.md) — a background daemon sweeps
  chunks whose bounds fall outside the declared retention window.
* [Continuous aggregates](./continuous-aggregates.md) — incremental
  materialised views that read only new chunks on each refresh.

Both are described in their own pages.

## When to use a hypertable vs alternatives

| Use case | Model |
|----------|-------|
| Pure time-ordered metrics, need downsampling | `CREATE TIMESERIES` (existing simpler surface) |
| Wide fact rows indexed by time, mix of TEXT/JSON/numeric | `CREATE HYPERTABLE` |
| Unstructured high-volume events with no time axis | Log Collections |
| Classic relational rows you still need UPDATE on | `CREATE TABLE` |

Hypertables are **automatically append-only**: attempting
`UPDATE` / `DELETE` fails at parse time. Use `drop_chunks` or a
retention policy for lifecycle management.

## Performance notes

* Chunk lookup is O(log n) through the temporal index — lookup cost
  is independent of the number of chunks.
* Each chunk compresses independently. The auto-selector picks
  `DeltaOfDelta` for monotonic timestamps, `T64` for narrow-range
  integers, `Raw + zstd` otherwise.
* `drop_chunks` is an O(1) metadata update plus a single filesystem
  removal per chunk — it never scans rows.

## Caveats (sprint 1 scope)

* The SQL parser surface (`CREATE HYPERTABLE`, `show_chunks`,
  `drop_chunks` functions) lands behind the `olap` feature flag once
  B7 partition pruning wires into the planner proper. Meanwhile the
  hypertable registry is callable programmatically via
  `reddb::storage::timeseries::HypertableRegistry`.
* Multi-column partition keys (space + time) aren't supported yet;
  single-column `time` only. Space dimensions belong to follow-up
  work.
