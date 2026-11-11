# Storage Engine Architecture

RedDB uses a layered storage engine that handles all data models through a unified persistence layer.

## Layer Overview

```mermaid
flowchart TB
    subgraph Application
        QE[Query Engine]
        GE[Graph Engine]
        VE[Vector Engine]
    end

    subgraph Storage
        US[Unified Store]
        IM[Index Manager]
        SM[Schema Manager]
    end

    subgraph Physical
        PG[Pager]
        BT[B-Tree]
        WL[WAL]
        SC[SIEVE Cache]
        EN[Encryption]
        MM[Memory Map]
    end

    subgraph Disk
        DF[Database File .rdb]
        WF[WAL File]
        MF[Manifest]
    end

    QE --> US
    GE --> US
    VE --> US

    US --> IM
    US --> SM
    US --> PG

    PG --> BT
    PG --> WL
    PG --> SC
    PG --> EN
    PG --> MM

    BT --> DF
    WL --> WF
    SC --> DF
    EN --> DF
    MM --> DF
    PG --> MF
```

## Physical Layer

The physical layer manages durable file I/O:

- **Pager**: Page-based I/O with configurable page sizes
- **WAL**: Write-ahead log for crash recovery
- **SIEVE Cache**: Adaptive page cache (SIEVE eviction algorithm)
- **Memory Map**: Optional mmap for read-heavy workloads
- **Encryption**: AES-256-GCM per-page envelope (foundation code; not yet
  written by the live pager — see [Storage Engine Encryption](encryption.md)).
  The canonical envelope is owned by the `reddb-io-crypto` authority crate
  (ADR 0054); the page-0 encrypted-database header lives in `reddb-io-file`.

## Logical Catalog

The catalog tracks the database's logical structure:

- **Collections**: Named entity containers
- **Schema Manifests**: Column type definitions
- **Index Descriptors**: Index metadata and lifecycle state
- **Graph Projections**: Named graph views for analytics
- **Analytics Jobs**: Scheduled graph computation metadata

## Execution Layer

The execution layer processes queries across all data models:

- **Table Planner**: Cost-based optimizer for SELECT/INSERT/UPDATE/DELETE
- **Graph Engine**: Pattern matching, traversal, pathfinding, analytics
- **Vector Engine**: HNSW, IVF, PQ, hybrid search
- **Join Executor**: Nested-loop joins between tables

## Read Acceleration Path

All query protocols enter the same runtime statement path. An eligible result
hit returns before parsing, planning, or execution. A miss continues through
the executor, where the independent page cache can still avoid physical I/O.

```mermaid
flowchart LR
    P[HTTP / gRPC / RedWire / PostgreSQL] --> R[Runtime statement frame]
    R --> RC[Result cache adapter]
    RC -->|hit| O[Response envelope]
    RC -->|miss| Q[Parser / planner / executor]
    Q --> PC[SIEVE page cache]
    PC --> PG[Pager]
    Q --> B[Blob Cache put]
    B --> L1[L1 memory]
    B --> L2[L2 in the .rdb]
    L1 --> RC
    L2 --> RC
```

The result cache uses the reserved `runtime.result_cache` Blob Cache namespace.
The SIEVE page cache is not an L1 tier of Blob Cache: it caches database pages,
while Blob Cache stores exact-key byte values and encoded query results.

## Entity Model

All data in RedDB is stored as entities. Every entity has:

| Field | Description |
|:------|:------------|
| `entity_id` | Unique u64 identifier (auto-assigned) |
| `collection` | Collection name |
| `kind` | Entity kind (row, node, edge, vector, document, kv) |
| `payload` | Kind-specific data (fields, properties, dense vector, etc.) |
| `metadata` | Optional key-value metadata |

## File Layout

A persistent RedDB database consists of:

```
data/
  reddb.rdb          # Main database file (pages)
  reddb.rdb.wal      # Write-ahead log
  reddb.rdb.manifest  # Physical metadata manifest
```

## Page Structure

The database file is divided into fixed-size pages:

| Section | Purpose |
|:--------|:--------|
| Header page | Database metadata, version, configuration |
| Collection root pages | One root per collection |
| B-Tree pages | Index and data pages |
| Overflow pages | Large values that don't fit in a single page |
| Free list | Recycled page tracking |
