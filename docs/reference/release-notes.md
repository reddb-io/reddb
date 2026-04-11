# Release Notes

## v0.1.2 (Current)

### Corruption Defense (7 layers)

- **File lock** -- exclusive `flock` via `fs2` prevents concurrent writes to the same `.rdb` file
- **Double-write buffer** -- `.rdb-dwb` companion file protects against torn pages on power loss
- **Header shadow** -- `.rdb-hdr` auto-recovers corrupted database header (page 0)
- **Metadata shadow** -- `.rdb-meta` auto-recovers corrupted collection registry (page 1)
- **fsync discipline** -- `persist()` and `save_to_file()` now call `sync_all()`, not just `flush()`
- **Two-phase checkpoint** -- crash-safe WAL application with `checkpoint_in_progress` flag in header
- **Binary store V3** -- CRC32 footer + atomic write-to-temp-then-rename pattern

### Read Optimizations (6 new)

- **Result cache** -- identical SELECT queries return in <0.1ms; 30s TTL, 1000 entry limit, auto-invalidated on all writes via `cdc_emit()`
- **Plan cache** -- parsed query plans cached (1000 capacity, 1h TTL) to skip re-parsing
- **Index-assisted lookups** -- `find_index_for_column()` + `hash_lookup()` wired into the query executor for O(1) equality lookups
- **Bloom filter (PK only)** -- bloom filter hints restricted to `_entity_id`, `row_id`, `id`, `key` fields to prevent false negatives on general columns
- **Column projection pushdown** -- `runtime_table_record_from_entity_projected()` wired into filtered scan paths; only materializes requested columns
- **Segment entity_count() O(1)** -- direct `len() - deleted.len()` without constructing full SegmentStats

### Bulk Insert Performance

- **Binary bulk insert** via gRPC -- 241K-380K ops/sec with protobuf native types
- **Flat Vec storage** for segments -- eliminates HashMap overhead during bulk inserts
- **Columnar row storage** -- 62% memory reduction (3.2GB to 1.2GB for 1M rows)

### AI & LLM Features

- **ASK command** -- full RAG pipeline: SEARCH CONTEXT + schema enrichment + LLM synthesis
- **SEARCH CONTEXT** -- 3-tier search (field-value index, token index, global scan) with cross-ref expansion
- **11 AI providers** -- OpenAI, Anthropic, Groq, OpenRouter, Together, Venice, Ollama, DeepSeek, HuggingFace, Local (Candle), Custom
- **Auto embedding** -- `INSERT ... WITH AUTO EMBED` for automatic vector generation
- **ContextIndex** -- dedicated inverted index with token + field-value posting lists

### Replication & Backup

- **CDC (Change Data Capture)** -- circular buffer emitting ChangeEvents on all entity CRUD
- **Backup scheduler** -- configurable background thread for automated backups
- **WAL archiving** -- archive WAL segments to remote backend before truncation
- **Point-in-time recovery** -- framework for restoring to specific timestamps

### Configuration System

- **red_config** -- KV store with dot-notation keys (`red.ai.default.provider`)
- **GET/POST /config** -- HTTP API for reading and writing configuration
- **JSON import/export** -- bidirectional conversion between flat KV and nested JSON

### Wire Protocol (TCP)

- Binary wire protocol for high-throughput client connections
- TLS support with auto-generated self-signed certificates for development
- Custom framing with message types for queries, bulk inserts, and results

### Documentation

- Complete `.rdb` file format specification (48 value types, page structure, WAL format)
- End-to-end tutorials for ASK, CONTEXT, multi-model queries
- File format anatomy with byte-level layout

---

## v0.1.0

Initial public release of RedDB.

### Core Engine

- Unified entity model for rows, documents, nodes, edges, vectors, and KV
- Page-based persistent storage with B-Tree indexes
- WAL-based crash recovery
- SIEVE cache for page eviction
- AES-256-GCM encryption at rest (optional)

### Query Engine

- SQL-like query syntax (SELECT, INSERT, UPDATE, DELETE, CREATE TABLE, DROP TABLE, ALTER TABLE)
- Universal query (`FROM ANY`) across all entity types
- Cost-based query planner and optimizer
- Gremlin, SPARQL, and natural language query modes

### Vector Engine

- HNSW approximate nearest neighbor search
- IVF with k-means clustering
- Product quantization, binary quantization, int8 quantization
- SIMD-accelerated distance computation
- Tiered search (automatic strategy selection)
- Hybrid search (text + vector + metadata)

### Graph Engine

- Labeled property graph model
- BFS and DFS traversal
- Shortest path (BFS, Dijkstra)
- Centrality (degree, closeness, betweenness, eigenvector, PageRank)
- Community detection (Louvain, label propagation)
- HITS, personalized PageRank
- Connected components, cycle detection, topological sort
- Clustering coefficient
- Named graph projections

### APIs

- HTTP REST API (97+ endpoints)
- gRPC API (116 RPCs)
- MCP server (29 tools for AI agents)
- CLI (`red` binary with 12 commands)
- Embedded Rust API

### Auth & Security

- User management with roles (admin, write, read)
- API keys and session tokens
- Encrypted vault for auth data
- Password hashing

### Operations

- Health, readiness, and stats endpoints
- Manifest, roots, snapshots, and exports
- Retention policies
- Checkpoint control
- Catalog consistency checks
- Index lifecycle management
- Physical state inspection and repair

### Deployment

- Embedded mode (Rust library)
- Server mode (HTTP/gRPC)
- Serverless mode (attach/warmup/reclaim)
- Primary-replica replication
- Docker support
- Systemd service installer

### Type System

- 48 native data types
- Network types (IP, MAC, CIDR, subnet, port)
- Temporal types (timestamp, date, time, duration)
- Geo types (latitude, longitude, GeoPoint)
- Locale types (country, language, currency)
- Reference types (NodeRef, EdgeRef, VectorRef, RowRef, etc.)
- Automatic type coercion and validation
