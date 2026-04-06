# reddb

<p align="center">
  <img width="180" src="https://img.icons8.com/fluency/240/database.png" alt="reddb">
</p>

<p align="center">
  <strong>Save tables, docs, binaries, graphs, vectors and embeddings in one database</strong><br>
  <em>one engine • one runtime • one operational surface • every important data shape</em>
</p>

<p align="center">
  <strong>reddb</strong> is a multi-structure database engine for applications that need
  structured rows, raw payloads, linked entities, semantic retrieval, graph analytics,
  operational metadata and exports without splitting data across multiple systems.
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

## Save anything

`reddb` is built for systems that do not fit into a single shape.

<table>
<tr>
<td width="33%">
<strong>Structured</strong><br><br>

- rows
- tables
- typed values
- scans
- filters
- joins

</td>
<td width="33%">
<strong>Semi-structured</strong><br><br>

- documents
- JSON-like payloads
- metadata
- binary blobs
- exports
- snapshots

</td>
<td width="33%">
<strong>Connected + semantic</strong><br><br>

- graph nodes
- graph edges
- paths
- vectors
- embeddings
- hybrid search

</td>
</tr>
</table>

What this means in practice:

- store application state, content, relationships and retrieval data together
- move from simple rows to graph analytics without changing databases
- keep local embedded access and still expose a proper server runtime
- persist operational state, manifests, roots, snapshots and exports in the same system

---

## Quick start

### Embedded

Use `reddb` directly inside your Rust process.

#### 1. Create a database handle

```rust
use reddb::{RedDB, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::new();

    let host_id = db
        .row(
            "hosts",
            vec![
                ("ip", Value::Text("10.0.0.1".into())),
                ("os", Value::Text("linux".into())),
                ("critical", Value::Boolean(true)),
            ],
        )
        .save()?;

    let node_id = db
        .node("graph", "Host")
        .node_type("host")
        .property("ip", "10.0.0.1")
        .save()?;

    let vector_id = db
        .vector("embeddings")
        .dense(vec![0.12, 0.91, 0.44])
        .content("host 10.0.0.1 running ssh")
        .save()?;

    println!("host={host_id} node={node_id} vector={vector_id}");
    Ok(())
}
```

#### 2. What happened

In a few lines, the same database stored:

- a table row
- a graph node
- a vector embedding

No extra services. No separate graph store. No separate vector engine.

### Server

Run `reddb` as an HTTP server.

#### 1. Start the server

```bash
reddb --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

#### 2. Create a row

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "ip": "10.0.0.1",
      "os": "linux",
      "critical": true
    },
    "metadata": {
      "source": "quickstart"
    }
  }'
```

#### 3. Create a node

```bash
curl -X POST http://127.0.0.1:8080/collections/graph/nodes \
  -H 'content-type: application/json' \
  -d '{
    "label": "Host",
    "node_type": "host",
    "properties": {
      "ip": "10.0.0.1"
    }
  }'
```

#### 4. Create a vector

```bash
curl -X POST http://127.0.0.1:8080/collections/embeddings/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.91, 0.44],
    "content": "host 10.0.0.1 running ssh",
    "metadata": {
      "kind": "host_embedding"
    }
  }'
```

#### 5. Run a query

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{
    "query": "FROM hosts h WHERE h.os = '\''linux'\'' ORDER BY h.ip LIMIT 10"
  }'
```

---

## A real multi-structure flow

This is the shape `reddb` is built for:

| Need | Store it as |
| --- | --- |
| operational facts | rows |
| rich object payloads | documents / JSON-like values |
| raw files or opaque bytes | binary payloads |
| linked entities | graph nodes and edges |
| semantic retrieval | vectors and embeddings |
| search context | metadata |
| operational durability state | manifests, roots, snapshots and exports |

And this is the point:

- one application can write a row
- link it to a graph node
- attach one or more embeddings
- run structured queries
- run graph traversal
- run vector or hybrid retrieval
- export and snapshot the same dataset

without changing databases halfway through the system design.

---

## What is `reddb`?

`reddb` is a standalone Rust database engine for multi-structure workloads.

It is not trying to be “just SQL”, “just document”, or “just vector”.
It is a **multi-structure database core** with one persistence layer and one operational surface for:

- structured rows and scans
- semi-structured documents
- graph nodes, edges, traversals and analytics
- dense vector search, IVF and hybrid retrieval
- physical metadata, manifests, snapshots and exports

`reddb` is designed to feel like one coherent system:

- one engine
- one runtime
- one operational surface
- multiple native data shapes

---

## Why `reddb`

Most storage stacks get awkward the moment your application needs more than one structure.

You start with rows.
Then you need metadata-heavy docs.
Then graph relationships.
Then embeddings.
Then hybrid search.
Then operational metadata.
Then exports, scans, health and online maintenance.

`reddb` is built so all of that belongs to the same system from day one.

- rows, docs, graph and vectors live in one engine
- one transaction boundary can touch multiple structures
- one runtime exposes scans, queries, analytics and operations
- one physical metadata story tracks snapshots, roots, manifests and exports

---

## What makes it special

### One database, not four glued together

- table data
- document-like payloads
- graph entities and traversals
- vector retrieval and hybrid ranking

All of these are first-class.

### Embedded-first, server-capable

Use `reddb` directly as a Rust crate inside your process, or run it as a server.

- low-latency local access
- no mandatory network hop
- clean server surface when you do want remote access

### Operational by default

- health endpoints
- runtime stats
- manifests
- collection roots
- snapshots
- exports
- retention controls
- maintenance and checkpointing

### Search that crosses structures

- text search
- vector search
- IVF search
- hybrid search
- graph-aware traversal and analytics

### Analytics built into the graph layer

- shortest path
- traversals
- components
- centrality
- communities
- clustering
- cycles
- topological sort

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

## Feature matrix

| Area | What `reddb` already exposes |
| --- | --- |
| Storage | rows, graph entities, vectors, paged persistence, metadata sidecar |
| Query | table, join, graph, path, vector and hybrid execution |
| Search | text, similarity, IVF, hybrid |
| Graph | traversals, pathfinding, centrality, communities, clustering, cycles |
| Operations | health, stats, manifest, roots, snapshots, exports, retention |
| Runtime | embedded runtime, connection pool, HTTP server |

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

The physical side is still evolving toward a tighter root-publication model. The repo already persists operational metadata, roots, manifests, snapshots and exports, but the final publication path is still being hardened.

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

`reddb` is built around one principle:

- one storage engine
- one runtime
- one operational story
- multiple native data shapes

Rows, docs, graphs and vectors should feel like different faces of the same database.

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
