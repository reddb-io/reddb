# Data Model Overview

RedDB exposes multiple kinds of "structure", and they are not all the same thing.

## Which model fits my use case?

```text
  Is the data a sequence of timestamped measurements?
  ├─ yes ─── Query by time window + aggregate?
  │          ├─ yes ─── [Time-Series] + [Hypertables] + [Continuous Aggregates]
  │          └─ no ──── [Time-Series] (raw append + downsample)
  │
  Is the data a point-in-time record with a stable schema?
  ├─ yes ─── Need typed columns + joins?
  │          ├─ yes ─── [Tables]
  │          └─ no, schema-free payload ─── [Documents]
  │
  Is the access pattern key → value?
  ├─ yes ─── [Key-Value]
  │
  Is it a queue of jobs / events for workers?
  ├─ yes ─── [Queues]
  │
  Do you need similarity search over embeddings?
  ├─ yes ─── [Vectors]  (optionally combined with [Graphs] for RAG)
  │
  Are relationships between entities first-class?
  ├─ yes ─── [Graphs]
```

Most non-trivial workloads combine two or three of these on the same
data; see [Competitive Positioning](/architecture/competitive-positioning.md)
for why that's the RedDB advantage.

The most important naming rule is this: a `collection` is the named logical container, while tables,
documents, KV, graphs, vectors, time-series, and queues are user-facing data models or semantics used
on top of collections. RedDB does not use a hierarchy where a collection contains multiple tables and
documents as separate child objects in the traditional sense.

For application design, think in **user-facing data models** such as tables, documents, queues, and time-series.
For engine work, compatibility checks, and storage refactors, it also matters to distinguish those models from
the **native persisted entity kinds** in the unified storage core and from **supporting engine structures** such as
indexes and probabilistic sketches.

## The Three Layers

| Layer | What it means | Examples |
|:------|:--------------|:---------|
| User-facing data models | The structures application code and query authors work with directly | Tables, Documents, KV, Graphs, Vectors, Time-Series, Queues |
| Native persisted entity kinds | The core entity shapes stored by the unified engine | `TableRow`, `GraphNode`, `GraphEdge`, `Vector`, `TimeSeriesPoint`, `QueueMessage` |
| Supporting engine structures | Persistent or runtime objects that accelerate or extend the engine, but are not collection entities | Indexes, HyperLogLog, Count-Min Sketch, Cuckoo Filter |

## Collection First Mental Model

Use this mental model throughout the docs:

- `users` is a collection that you might use as a table.
- `events` is a collection that you might use for documents.
- `config` is a collection that you might use as KV.
- Some collection combinations can mix shapes, depending on the model.

This is why many APIs are shaped like `/collections/{name}/rows`, `/collections/{name}/documents`, or
`/collections/{name}/vectors`: the collection name stays stable, while the endpoint expresses the model
semantics for the entities you are writing or reading.

## User-Facing Data Models

These are the main structures most users mean when they ask "what can I build in RedDB?"

| Model | Main write path | Main query path | Best for | Native backing |
|:------|:----------------|:----------------|:---------|:---------------|
| Tables & Rows | `CREATE TABLE`, `INSERT INTO users (...)` | `SELECT`, `UPDATE`, `DELETE` | Structured records with typed fields | Native `TableRow` |
| Documents | `INSERT INTO logs DOCUMENT (body) VALUES (...)` | `SELECT`, `FROM ANY`, field projections | Flexible JSON payloads | User-facing model over the unified collection path |
| Key-Value | `INSERT INTO config KV (key, value) VALUES (...)` or KV HTTP API | KV API, `SELECT key, value FROM config` | Config, sessions, flags, cache-like lookups | User-facing model over the unified collection path |
| Graphs | `INSERT INTO network NODE ...`, `INSERT INTO network EDGE ...` | `MATCH`, `GRAPH ...`, `PATH ...` | Relationships, traversals, analytics | Native `GraphNode` and `GraphEdge` |
| Vectors | `INSERT INTO embeddings VECTOR (...)` | `VECTOR SEARCH`, `HYBRID ... VECTOR SEARCH` | Embeddings, semantic retrieval, RAG | Native `Vector` |
| Time-Series | `CREATE TIMESERIES`, `INSERT INTO cpu_metrics (...)` | `SELECT`, `time_bucket(...)`, range filters | Metrics, telemetry, sensor data | Native `TimeSeriesPoint` |
| Queues & Deques | `CREATE QUEUE`, `QUEUE PUSH`, `QUEUE READ`, `QUEUE ACK` | Queue commands and consumer-group flow | Job queues, retries, DLQ, work distribution | Native `QueueMessage` |

## Native Persisted Entity Kinds

At the storage-core layer, RedDB currently persists these native entity kinds:

- `TableRow`
- `GraphNode`
- `GraphEdge`
- `Vector`
- `TimeSeriesPoint`
- `QueueMessage`

These are the shapes that matter most when you are reviewing:

- file-format compatibility
- checkpoint / reopen behavior
- collection recovery paths
- storage-engine refactors
- regressions caused by changes in unified persistence

## The Important Distinction for Documents and KV

Documents and KV are first-class **user-facing** models, but they do **not** currently introduce separate native
entity kinds in the storage core.

That distinction is important because it explains how regressions usually spread:

- Refactors in the generic row / unified collection plumbing can affect **tables, documents, and KV** together.
- Refactors in native queue persistence tend to affect **queues** directly.
- Refactors in native point storage tend to affect **time-series** directly.
- Graph and vector issues usually stay closer to their own native entity kinds.

If you are auditing backward compatibility after a storage refactor, this is one of the first distinctions to keep
in mind.

## Supporting Structures You Can Also Build

RedDB also lets you create important supporting structures that are not collection entities themselves.

### Indexes

Indexes accelerate reads and are attached to collections / columns rather than stored as user entities.

| Structure | Syntax | Best for |
|:----------|:-------|:---------|
| B-Tree | `CREATE INDEX idx_created ON events (created_at)` | General ordered lookups and ranges |
| Hash | `CREATE INDEX idx_email ON users (email) USING HASH` | Exact-match lookups |
| Bitmap | `CREATE INDEX idx_status ON orders (status) USING BITMAP` | Low-cardinality analytical filters |
| R-Tree | `CREATE INDEX idx_location ON sites (location) USING RTREE` | Spatial search |
| Context Index | `CREATE TABLE ... WITH CONTEXT INDEX ON (host, email)` | Cross-model context search and identity resolution |

### Probabilistic Structures

These are first-class commands and durable engine objects, but they are not collection entity kinds.

| Structure | Syntax | Best for |
|:----------|:-------|:---------|
| HyperLogLog | `CREATE HLL visitors` | Approximate distinct counts |
| Count-Min Sketch | `CREATE SKETCH clicks WIDTH 2000 DEPTH 7` | Approximate frequency estimation |
| Cuckoo Filter | `CREATE FILTER sessions CAPACITY 500000` | Approximate membership with deletion |

## Quick Selection Guide

Use this as the fast decision tree:

- Need typed columns, constraints, and conventional SQL: use **Tables & Rows**.
- Need tables where UPDATE/DELETE must be rejected (audit, ledger, events): use **Append-Only Tables**.
- Need flexible JSON payloads: use **Documents**.
- Need direct lookup by key: use **Key-Value**.
- Need relationships, traversals, or graph analytics: use **Graphs**.
- Need embedding similarity or semantic retrieval: use **Vectors**.
- Need timestamp-first metrics with retention and downsampling: use **Time-Series**.
- Need automatic chunk partitioning + `drop_chunks` + partition TTL: use **Hypertables**.
- Need pre-aggregated dashboards with incremental refresh: use **Continuous Aggregates**.
- Need job processing, retries, DLQ, or consumer groups: use **Queues & Deques**.
- Need approximate counting or membership at low memory cost: use **Probabilistic Structures**.

## See Also

- [Tables & Rows](/data-models/tables.md)
- [Append-Only Tables](/data-models/append-only-tables.md)
- [Documents](/data-models/documents.md)
- [Key-Value](/data-models/key-value.md)
- [Graphs](/data-models/graphs.md)
- [Vectors & Embeddings](/data-models/vectors.md)
- [Time-Series](/data-models/timeseries.md)
- [Hypertables](/data-models/hypertables.md)
- [Continuous Aggregates](/data-models/continuous-aggregates.md)
- [Partition TTL](/data-models/partition-ttl.md)
- [Queues & Deques](/data-models/queues.md)
- [Probabilistic Structures](/data-models/probabilistic.md)
- [CREATE INDEX](/query/create-index.md)
- [Using RedDB for Logs](/guides/using-reddb-for-logs.md)
