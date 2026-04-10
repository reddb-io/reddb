# Configuration

RedDB is configured through three layers: CLI flags, environment variables, and the `red_config` KV store. All runtime settings can be managed via HTTP without restart.

## Server Flags

```bash
red server [flags]
```

| Flag | Short | Description | Default |
|:-----|:------|:------------|:--------|
| `--path` | `-d` | Persistent database file path (omit for in-memory) | `./data/reddb.rdb` |
| `--bind` | `-b` | Legacy single-transport bind address `host:port` | `127.0.0.1:50051` |
| `--grpc` | | Enable the gRPC API | disabled unless selected explicitly or by default fallback |
| `--http` | | Enable the HTTP API | `false` |
| `--grpc-bind` | | Explicit gRPC bind address `host:port` | |
| `--http-bind` | | Explicit HTTP bind address `host:port` | |
| `--role` | `-r` | Replication role: `standalone`, `primary`, `replica` | `standalone` |
| `--primary-addr` | | Primary gRPC address for replica mode | |
| `--read-only` | | Open database in read-only mode | `false` |
| `--no-create-if-missing` | | Fail if database file doesn't exist | `false` |
| `--vault` | | Enable encrypted auth vault | `false` |

Recommended pattern:

```bash
red server \
  --path ./data/reddb.rdb \
  --grpc-bind 127.0.0.1:50051 \
  --http-bind 127.0.0.1:8080
```

Use `--bind` only when you want a single transport and do not care about the other one.

## Storage Modes

### In-Memory

Omit `--path` to run entirely in RAM. Useful for development and testing:

```bash
red server --http --bind 127.0.0.1:8080
```

### Persistent (File-Backed)

Specify `--path` to persist data to disk with WAL-based durability:

```bash
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080
```

### Read-Only

Open an existing database without writes:

```bash
red server --http --path ./data/reddb.rdb --read-only --bind 127.0.0.1:8080
```

## Deployment Profiles

RedDB supports three deployment profiles, each with different operational characteristics:

| Profile | Use Case | Characteristics |
|:--------|:---------|:----------------|
| **Embedded** | Library usage inside a Rust process | Direct function calls, no network, lowest latency |
| **Server** | Standalone database server | HTTP/gRPC transport, connection pool, full operational surface |
| **Serverless** | Edge/function workloads | Attach/warmup/reclaim lifecycle, ephemeral connections |

Query the active profile:

```bash
curl http://127.0.0.1:8080/deployment/profiles?profile=server
```

## Replication

### Primary Mode

```bash
red server \
  --path ./data/primary.rdb \
  --role primary \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080
```

### Replica Mode

```bash
red replica \
  --primary-addr http://primary-host:50051 \
  --path ./data/replica.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080
```

Check replication status:

```bash
red status --bind primary-host:50051
```

## Global CLI Flags

These flags work with any `red` command:

| Flag | Short | Description |
|:-----|:------|:------------|
| `--help` | `-h` | Show help for the command |
| `--json` | `-j` | Force JSON output |
| `--output FORMAT` | `-o` | Output format: `text`, `json`, or `yaml` |
| `--verbose` | `-v` | Verbose output |
| `--no-color` | | Disable colored output |
| `--version` | | Show version |

## Runtime Configuration (`red_config`)

RedDB stores runtime settings in the `red_config` collection using dot-notation keys. Changes take effect immediately without restart.

### Import / Export

```bash
# Export all config as nested JSON
curl http://127.0.0.1:8080/config

# Import from JSON file
curl -X POST http://127.0.0.1:8080/config -d @examples/config.json

# Override specific values
curl -X POST http://127.0.0.1:8080/config \
  -d '{"red":{"ai":{"default":{"provider":"ollama","model":"llama3"}}}}'
```

See [`examples/config.json`](https://github.com/forattini-dev/reddb/blob/main/examples/config.json) for a complete example with all defaults.

### SQL Commands

You can read and write configuration directly from the query engine:

```sql
-- Set a config value
SET CONFIG red.ai.default.provider = 'groq'
SET CONFIG red.storage.hnsw.ef_search = 100

-- Show all config
SHOW CONFIG

-- Show config subtree
SHOW CONFIG red.ai
SHOW CONFIG red.storage.hnsw
```

### Individual Key API

Read, write, or delete a single config key via HTTP:

```bash
# Read a key or subtree
curl http://127.0.0.1:8080/config/red.ai.default.provider

# Set a key
curl -X PUT http://127.0.0.1:8080/config/red.ai.default.provider \
  -H 'content-type: application/json' \
  -d '{"value": "groq"}'

# Delete a key
curl -X DELETE http://127.0.0.1:8080/config/red.ai.default.model
```

### Resolution Order

For any setting, RedDB checks in order:

1. **Environment variable** (e.g., `REDDB_AI_PROVIDER`)
2. **`red_config` KV store** (e.g., `red.ai.default.provider`)
3. **Hardcoded default**

### AI & LLM (`red.ai.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.ai.default.provider` | `openai` | Default provider for ASK, SEARCH SIMILAR TEXT, AUTO EMBED |
| `red.ai.default.model` | provider default | Default model |
| `red.ai.{provider}.{alias}.key` | — | API key (e.g., `red.ai.groq.default.key`) |
| `red.ai.{provider}.{alias}.base_url` | — | Custom API base URL |
| `red.ai.max_embedding_inputs` | `256` | Max inputs per embedding batch |
| `red.ai.max_prompt_batch` | `256` | Max prompts per batch |
| `red.ai.timeout.connect_secs` | `10` | API connection timeout |
| `red.ai.timeout.read_secs` | `90` | API read timeout |

### Server (`red.server.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.server.max_scan_limit` | `1000` | Max rows in a single scan |
| `red.server.max_body_size` | `1048576` | Max HTTP body size (1 MB) |
| `red.server.read_timeout_ms` | `5000` | HTTP read timeout |
| `red.server.write_timeout_ms` | `5000` | HTTP write timeout |

### Storage (`red.storage.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.storage.page_size` | `4096` | Page size in bytes |
| `red.storage.page_cache_capacity` | `100000` | Page cache capacity |
| `red.storage.auto_checkpoint_pages` | `1000` | Checkpoint after N dirty pages |
| `red.storage.snapshot_retention` | `16` | Snapshots to keep |
| `red.storage.segment.max_entities` | `100000` | Entities per segment before sealing |
| `red.storage.segment.compression_level` | `6` | Compression level (0-9) |
| `red.storage.hnsw.m` | `16` | HNSW max connections per node |
| `red.storage.hnsw.ef_search` | `50` | HNSW query-time candidate list |
| `red.storage.ivf.n_lists` | `100` | IVF Voronoi cells |
| `red.storage.ivf.n_probes` | `10` | IVF cells to probe |
| `red.storage.bm25.k1` | `1.2` | BM25 TF saturation |
| `red.storage.bm25.b` | `0.75` | BM25 length normalization |

### Search & RAG (`red.search.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.search.rag.max_total_chunks` | `25` | Context chunks for LLM |
| `red.search.rag.similarity_threshold` | `0.8` | Vector similarity threshold |
| `red.search.rag.graph_depth` | `2` | Graph traversal depth |
| `red.search.fusion.vector_weight` | `0.5` | Vector weight in hybrid search |
| `red.search.fusion.graph_weight` | `0.3` | Graph weight in hybrid search |
| `red.search.fusion.dedup_threshold` | `0.85` | Deduplication similarity |

### Auth (`red.auth.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.auth.enabled` | `false` | Enable authentication |
| `red.auth.session_ttl_secs` | `3600` | Session TTL (1 hour) |
| `red.auth.require_auth` | `false` | Require auth for all operations |

### Backup & Recovery (`red.backup.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.backup.enabled` | `false` | Enable scheduled backups |
| `red.backup.interval_secs` | `3600` | Backup interval (1 hour) |
| `red.backup.retention_count` | `24` | Snapshots to keep |
| `red.backup.upload` | `false` | Auto-upload to remote backend |
| `red.backup.backend` | `local` | Backend: `local`, `s3`, `r2`, `turso`, `d1` |
| `red.wal.archive.enabled` | `false` | Archive WAL segments before truncation |
| `red.wal.archive.retention_hours` | `168` | WAL archive retention (7 days) |
| `red.wal.archive.prefix` | `wal/` | Remote key prefix for WAL segments |
| `red.cdc.enabled` | `true` | Enable change data capture |
| `red.cdc.buffer_size` | `100000` | CDC event buffer capacity |

### Query Engine (`red.query.*`)

| Key | Default | Description |
|:----|:--------|:------------|
| `red.query.connection_pool.max_connections` | `64` | Max connections |
| `red.query.connection_pool.max_idle` | `16` | Max idle connections |
| `red.query.max_recursion_depth` | `1000` | Max CTE recursion depth |

## Performance Tuning

The keys most relevant to query performance are spread across the storage and query sections above. Here they are in one place for quick reference:

| Key | Default | Description |
|:----|:--------|:------------|
| `red.storage.hnsw.ef_search` | `50` | HNSW query-time precision (higher = more accurate, slower) |
| `red.storage.hnsw.m` | `16` | HNSW max connections (higher = better recall, more memory) |
| `red.storage.segment.max_entities` | `100000` | Entities per segment before sealing |
| `red.query.connection_pool.max_connections` | `64` | Max concurrent query connections |

> [!TIP]
> RedDB automatically selects hash joins for large datasets (>10K cross-product) and parallel segment scanning for multi-segment collections. No configuration needed -- these optimizations kick in when the query planner detects they will help.

## Feature Flags (Compile-Time)

When using RedDB as a Rust crate, you control features at compile time:

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph", "encryption"] }
```

| Feature | What It Enables |
|:--------|:----------------|
| `query-vector` | Vector similarity search in the query engine |
| `query-graph` | Graph traversal and analytics in the query engine |
| `query-fulltext` | Full-text search indexing and queries |
| `encryption` | AES-256-GCM encryption at rest |
| `backend-s3` | S3-compatible remote storage backend |
| `backend-turso` | Turso (libSQL) remote backend |
| `backend-d1` | Cloudflare D1 remote backend |

> [!NOTE]
> By default, no optional features are enabled. The base crate provides table and document storage with the core query engine.
