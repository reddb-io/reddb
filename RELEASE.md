# RedDB v0.1.0 — Beta Release Notes

**Release date:** 2026-04-08
**Status:** Public Beta

---

## What is RedDB?

RedDB is a unified multi-model database engine for applications that need structured rows, documents, graphs, vectors, and key-value storage in a single runtime. It supports embedded, server (HTTP + gRPC), and serverless execution profiles.

---

## Features in v0.1.0

### Data Models
- **Tables/Rows**: CRUD with filter, projection, and ordering
- **Documents**: JSON-like fields with path-based filtering and `set`/`unset`/`replace` mutations
- **Vectors**: Dense vector storage with cosine/euclidean similarity search, HNSW and IVF indexes
- **Graphs**: Node/edge CRUD, path traversal (BFS/DFS/Dijkstra), centrality, community detection, cycle analysis
- **Key-Value**: Row-based KV pairs with standard CRUD

### Universal Query
- `SELECT * FROM any` returns mixed entity types in a single result set
- Filter by `_entity_type` and `_capabilities`
- Deterministic sort with stable tie-breaking for reproducible pagination
- Multi-mode parsing: SQL, Gremlin, SPARQL, natural language
- Cost-based query planner with cardinality estimation
- `EXPLAIN` with `is_universal` flag for mixed-mode queries

### Artifact Lifecycle
- Canonical state machine: `declared` -> `building` -> `ready` -> `stale` -> `failed` -> `requires_rebuild`
- `ArtifactState` enum with `is_queryable()`, `can_rebuild()`, `needs_attention()` helpers
- Artifact state exposed in catalog JSON and health reports
- Rebuild/warmup operations for HNSW, IVF, graph adjacency, text, and document path indexes

### Execution Profiles
- **Embedded**: In-process library, zero-copy access, in-memory or persistent
- **Server**: HTTP API + gRPC with typed protobuf contracts
- **Serverless**: `attach` / `warmup-eligible` / `reclaim` lifecycle

### Observability
- Health reports with `healthy` / `degraded` / `unhealthy` states
- Catalog snapshots with index status, artifact state, and attention scores
- Query explain with cost breakdown and logical plan tree
- Native physical state inspection (paged storage, metadata, registry)

### Storage Engine
- Page-based storage with B-tree indexing
- Write-ahead log (WAL) with checkpointing
- SIEVE cache eviction
- Bloom filters for negative lookups
- Encrypted pager support (AES-GCM)
- Spill-to-disk for large working sets

---

## Known Limitations

- No multi-region replication or automatic sharding
- No distributed query planner — single-node cost-based only
- No advanced RBAC — token-based authentication only
- No cross-entity transactions — per-collection atomicity
- Serverless profile is experimental
- gRPC uses JSON-wrap for some experimental endpoints

---

## Breaking Changes

This is the first public release. No backwards compatibility guarantees exist yet. The API surface may change in v0.2.0.

---

## Test Coverage

- 1,246 tests passing (1,238 unit + 8 integration), 0 failures
- 8 integration smoke tests covering all entity domains, universal query, artifact lifecycle, and health
- Deterministic pagination validated
- Zero compiler warnings, zero compiler errors

## Binary Size

- `reddb` CLI: 5.6 MB (release)
- `reddb-grpc` server: 8.8 MB (release)

---

## What's Next (v0.2.0 Roadmap)

- Formal profile equivalence certification
- Distributed query federation
- Streaming query results
- Enhanced RBAC and audit logging
- Performance benchmarks and optimization
