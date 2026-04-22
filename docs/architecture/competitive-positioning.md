# Competitive Positioning — RedDB vs TimescaleDB vs ClickHouse

We don't compete frontally with either system on their core
strength. We compete by bundling 80% of both + a multi-model +
AI-native engine they don't have.

## The three products

| System        | Core strength                                  | Weakness for our audience |
|---------------|------------------------------------------------|---------------------------|
| TimescaleDB   | Time-series on PostgreSQL, rich PG ecosystem   | Row-store OLAP; no native vectors / graph / ML |
| ClickHouse    | Columnar OLAP; petabyte scale; SIMD execution  | Single model; AI / vectors / graph are bolt-ons |
| RedDB         | Unified: tables + docs + vectors + graph + TS + KV + queue + logs + ML | Younger; OLAP SIMD still catching up |

## What RedDB does that neither does

1. **Multi-model in one engine.** A single SQL query can join a
   table, a graph traversal, and a vector similarity — no federated
   query layer, no ETL to a second store.
2. **AI-first.** HNSW / IVF vector indexes, hybrid BM25 + RRF
   retrieval, semantic cache, classifier + symbolic regression,
   re-ranking — all first-class SQL surface.
3. **Log + queue models.** Append-only log collections run sub-ms;
   queues with consumer groups live in the same catalog. You don't
   need Kafka in front of your DB for most small/medium workloads.
4. **Embedded or distributed.** The same binary runs single-node
   (Rust crate or server) or scales out via WAL replication + quorum.

## What we're closing in on them

Tracked in the [12-week TS/CH parity plan](../../../.claude/plans/eu-recebi-esta-review-serene-glade.md):

### TimescaleDB parity

| Feature                    | Status |
|----------------------------|--------|
| Delta-of-Delta + XOR TS codecs | Shipped (`src/storage/timeseries/compression.rs`) |
| T64 bit-packing + zstd fallback | Shipped (A4) |
| Temporal index (BRIN-style)   | Shipped |
| Chunks (open/sealed, bloom, zone maps) | Shipped |
| Hypertables (chunking + router) | Registry shipped (A1); SQL DDL next sprint |
| Retention daemon              | Shipped (A3) |
| Continuous aggregates         | Engine shipped (A2); SQL wiring next |
| Drop chunks / show chunks API | Shipped programmatically |

### ClickHouse parity

| Feature                                 | Status |
|-----------------------------------------|--------|
| Columnar batch execution (2048-row vectors) | Shipped (B1) |
| SIMD reducers (sum / min / max / filter_gt) | Shipped (B2) |
| Per-column codecs (LZ4 / ZSTD / Delta / Dict) | Shipped (B3) |
| Aggregate functions (quantileTDigest, uniq, corr, count_if, groupArray) | Shipped (B4) |
| Parallelism in executors (rayon)         | Shipped (B6) |
| Partition pruning (RANGE / LIST / HASH)  | Shipped (B7) |
| Projections (query-specific pre-aggregation) | Matcher shipped (B5); maintenance next sprint |
| Merge tree with level tiers              | Out of scope (our pager + btree covers OLAP cases) |

### Where we stay behind for now

* **Distributed query / sharding** — single-node for this cycle.
  Foundations (serialisable `ColumnBatch`, WAL replication, quorum)
  are in place; the router + Raft catalog land in a follow-on
  trimestre. Documented in [distributed-roadmap.md](./distributed-roadmap.md).
* **Mature PG extension ecosystem** — not feasible without forking
  PostgreSQL. We add selected features natively (hypertables,
  continuous aggregates, pgvector surface) instead of importing the
  full extension tree.
* **GPU inference / ONNX runtime** — ML features are CPU-first with
  Anthropic / Cohere / OpenAI / local-ollama providers. GPU path is
  a feature-flag item on the ML roadmap, not here.

## Pitch

> **"TimescaleDB + ClickHouse + pgvector + Feast + PySR + Cohere →
> RedDB."**
>
> You replace six systems with one engine that speaks SQL, scales
> on one or many nodes, and lets you train classifiers, run symbolic
> regression, and cache semantic answers in the same query pipeline.
