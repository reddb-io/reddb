# redDB

<p align="center">
  <img width="180" src="https://img.icons8.com/fluency/240/database.png" alt="reddb">
</p>

<p align="center">
  <strong>The multi-structure database engine for rows, docs, graphs and vectors</strong><br>
  <em>TigerBeetle-inspired physical design • graph analytics • vector search • embedded-first runtime</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/crate-reddb-c0392b?style=flat" alt="crate">
  &nbsp;
  <img src="https://img.shields.io/badge/version-0.1.0-111111?style=flat" alt="version">
  &nbsp;
  <img src="https://img.shields.io/badge/language-Rust-b7410e?style=flat&logo=rust" alt="rust">
  &nbsp;
  <img src="https://img.shields.io/badge/model-embedded--first-0f766e?style=flat" alt="embedded">
  &nbsp;
  <img src="https://img.shields.io/badge/runtime-HTTP%20API-2563eb?style=flat" alt="http">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/data%20models-tables%20%C2%B7%20docs%20%C2%B7%20graphs%20%C2%B7%20vectors-dc2626?style=flat" alt="models">
  &nbsp;
  <img src="https://img.shields.io/badge/search-vector%20%C2%B7%20hybrid%20%C2%B7%20text-7c3aed?style=flat" alt="search">
  &nbsp;
  <img src="https://img.shields.io/badge/graph-paths%20%C2%B7%20centrality%20%C2%B7%20communities-0891b2?style=flat" alt="graph">
</p>

---

## What is `reddb`?

`reddb` is the storage engine extracted from `redblue` and rebuilt as a standalone Rust crate.

It is not trying to be “just SQL”, “just document”, or “just vector”.
It is a **multi-structure database core** with one persistence layer and one operational surface for:

- structured rows and scans
- semi-structured documents
- graph nodes, edges, traversals and analytics
- dense vector search, IVF and hybrid retrieval
- physical metadata, manifests, snapshots and exports

The design direction is deliberate:

- **TigerBeetle** for physical discipline, publication of state, manifest trail and deterministic storage mindset
- **MariaDB / libSQL** for runtime surface, query ergonomics and operational clarity
- **pgvector / Qdrant / Weaviate** for vector search, IVF, ANN and hybrid ranking
- **Neo4j** for graph traversal, projections and analytics workflows

---

## Why this exists

`redblue` had a very powerful storage subsystem, but it got too heavy because the database lived inside the application.

This repo exists to:

- make the database independent
- publish it as its own Cargo crate
- evolve the storage engine faster
- migrate `redblue` to consume `reddb` as a clean dependency

---

## Current capabilities

### Core engine

- unified entity model
- persistence for rows, graph entities and vectors
- paged backend support
- physical metadata sidecar
- manifest trail and collection roots
- snapshots and named exports
- retention policy for snapshots and exports
- health diagnostics and runtime stats

### Query/runtime

- embedded runtime with connection pool
- HTTP server surface
- collection scans
- table query execution in `/query`
- join execution in `/query`
- graph query execution in `/query`
- path query execution in `/query`
- vector query execution in `/query`
- hybrid query execution in `/query`

### Vector

- similarity search
- IVF search
- k-means-backed IVF training on demand
- hybrid search
- text/doc search API
- vector metadata filtering in runtime query path

### Graph

- neighborhood expansion
- BFS / DFS traversal
- shortest path
- connected / weak / strong components
- degree / closeness / betweenness / eigenvector centrality
- PageRank and personalized PageRank
- HITS
- Louvain and label propagation
- clustering coefficient
- cycle discovery
- topological sort
- named graph projections
- persisted analytics job metadata

### Operations

- `GET /health`
- `GET /ready`
- `GET /stats`
- `GET /catalog`
- `GET /manifest`
- `GET /roots`
- `GET /snapshots`
- `GET /exports`
- `GET /indexes`
- `GET /graph/projections`
- `GET /graph/jobs`

---

## Architecture direction

`reddb` is being shaped as a layered database engine:

1. **Physical layer**
   - durable file layout
   - metadata manifest
   - snapshots
   - exports
   - collection roots

2. **Logical catalog**
   - collections
   - schema manifests
   - index descriptors
   - graph projections
   - analytics jobs

3. **Execution layer**
   - scans
   - table filters
   - joins
   - graph traversal
   - vector retrieval
   - hybrid ranking

4. **Operational surface**
   - embedded runtime
   - HTTP API
   - health
   - stats
   - maintenance
   - checkpointing

The physical side is still in transition toward a more TigerBeetle-style root-publication design. Right now the repo already persists operational metadata and roots, but the final superblock/WAL publication model is still being completed.

---

## Example API surface

### Health

```bash
curl http://127.0.0.1:8080/health
```

### Query

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{
    "query": "FROM hosts h WHERE h.os = '\''linux'\'' ORDER BY h.ip LIMIT 10"
  }'
```

### Vector search

```bash
curl -X POST http://127.0.0.1:8080/collections/embeddings/similar \
  -H 'content-type: application/json' \
  -d '{
    "vector": [0.12, 0.91, 0.44],
    "k": 10,
    "min_score": 0.2
  }'
```

### Hybrid search

```bash
curl -X POST http://127.0.0.1:8080/hybrid/search \
  -H 'content-type: application/json' \
  -d '{
    "vector": [0.12, 0.91, 0.44],
    "collections": ["hosts", "embeddings"],
    "limit": 20,
    "weights": {
      "vector": 0.6,
      "graph": 0.2,
      "filter": 0.2
    }
  }'
```

### Graph analytics

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{
    "algorithm": "pagerank",
    "top_k": 20,
    "alpha": 0.85,
    "epsilon": 0.000001,
    "max_iterations": 100
  }'
```

---

## Repo status

This repository is already beyond “scaffold”, but it is **not at 1.0 shape yet**.

What is already strong:

- storage extraction is complete
- runtime/API surface is broad
- graph and vector capabilities are real
- operational metadata exists and is queryable

What still needs to harden:

- final physical publication model
- persistent binary index formats
- stronger SQL/table planner and executor depth
- gRPC surface
- replication and log shipping

---

## Philosophy

Most databases pick one dominant model and bolt the rest on later.

`reddb` is taking the opposite route:

- one storage engine
- one runtime
- one operational story
- multiple native data shapes

Rows, docs, graphs and vectors should not feel like four products awkwardly glued together.

That is the bar.

---

## Crate

`Cargo.toml`

```toml
[package]
name = "reddb"
version = "0.1.0"
edition = "2021"
```

Current feature flags:

- `query-vector`
- `query-graph`
- `query-fulltext`
- `encryption`

---

## License

This repo is currently in active design and extraction mode. Add the final project license before publishing the crate.
