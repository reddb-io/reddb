# Release Notes

## v0.1.0 (Current)

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
