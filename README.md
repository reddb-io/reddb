<p align="center">
  <h1 align="center">RedDB</h1>
  <p align="center"><strong>The AI-first multi-model database.</strong></p>
  <p align="center">Tables. Documents. Graphs. Vectors. KV. One engine. Ask it anything.</p>
</p>

<p align="center">
  <a href="https://github.com/forattini-dev/reddb/releases"><img src="https://img.shields.io/github/v/release/forattini-dev/reddb?style=flat-square" alt="Release"></a>
  <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square" alt="License"></a>
  <a href="https://www.npmjs.com/package/reddb-cli"><img src="https://img.shields.io/npm/v/reddb-cli?style=flat-square&label=npm" alt="npm"></a>
</p>

---

## The Killer Feature: `ASK`

```sql
ASK 'who owns passport AB1234567 and what services do they use?'
```

One command. RedDB searches across tables, graphs, vectors, documents, and key-value stores -- builds context -- calls an LLM -- returns a natural-language answer. No pipelines. No glue code. No other database does this.

---

## 7 Data Models, 1 Engine

Stop running Postgres + Neo4j + Pinecone + Redis + Mongo + InfluxDB + RabbitMQ. RedDB unifies them.

```sql
-- Relational rows
INSERT INTO users (name, email) VALUES ('Alice', 'alice@co.com')

-- JSON documents
INSERT INTO logs DOCUMENT (body) VALUES ('{"level":"info","msg":"login"}')

-- Graph edges
INSERT INTO network EDGE (label, from, to) VALUES ('CONNECTS', 1, 2)

-- Vector similarity search
SEARCH SIMILAR TEXT 'anomaly detected' COLLECTION events

-- Key-value
PUT config.theme = 'dark'

-- Time-series metrics (with retention & downsampling)
CREATE TIMESERIES cpu_metrics RETENTION 90 d
INSERT INTO cpu_metrics (metric, value, tags) VALUES ('cpu.idle', 95.2, '{"host":"srv1"}')

-- Message queues (FIFO, priority, consumer groups)
CREATE QUEUE tasks MAX_SIZE 10000
QUEUE PUSH tasks '{"job":"process","id":123}'
QUEUE POP tasks
```

Same file. Same engine. Same query language.

---

## AI-Native From Day One

```sql
-- Semantic search without managing vectors yourself
SEARCH SIMILAR TEXT 'suspicious login' COLLECTION logs USING groq

-- Auto-embed on insert -- vectors are created for you
INSERT INTO articles (title, body) VALUES ('AI Safety', 'Alignment research...')
  WITH AUTO EMBED (body) USING openai

-- Context search: find everything related to an entity across all models
SEARCH CONTEXT '192.168.1.1' FIELD ip DEPTH 2

-- Ask questions in plain English
ASK 'what vulnerabilities affect host 10.0.0.1?' USING anthropic
```

RedDB retrieves context from every data model, feeds it to the LLM, and gives you a grounded answer. RAG built into the database layer.

---

## 11 AI Providers

Swap providers with a keyword. No code changes.

| Provider | Keyword | API Key Required |
|:---------|:--------|:-----------------|
| OpenAI | `openai` | Yes |
| Anthropic | `anthropic` | Yes |
| Groq | `groq` | Yes |
| OpenRouter | `openrouter` | Yes |
| Together | `together` | Yes |
| Venice | `venice` | Yes |
| DeepSeek | `deepseek` | Yes |
| HuggingFace | `huggingface` | Yes |
| Ollama | `ollama` | No (local) |
| Local | `local` | No |
| Custom URL | `https://...` | Configurable |

```sql
ASK 'summarize alerts' USING groq MODEL 'llama-3.3-70b-versatile'
ASK 'summarize alerts' USING ollama MODEL 'llama3'
ASK 'summarize alerts' USING anthropic
```

Set a default provider so you can drop `USING` from every query:

```bash
# Set default provider -- no more USING on every query
curl -X POST http://127.0.0.1:8080/ai/credentials \
  -d '{"provider":"groq","api_key":"gsk_xxx","default":true}'
```

```sql
-- Now ASK uses groq by default
ASK 'what happened?'
```

```bash
# Export/import all config as JSON
curl http://127.0.0.1:8080/config
```

---

## Probabilistic Data Structures

Built-in approximate data structures for real-time analytics at scale.

```sql
-- HyperLogLog: count unique visitors (~0.8% error, ~16KB memory)
CREATE HLL visitors
HLL ADD visitors 'user1' 'user2' 'user3'
HLL COUNT visitors

-- Count-Min Sketch: frequency estimation
CREATE SKETCH click_counter WIDTH 2000 DEPTH 7
SKETCH ADD click_counter 'button_a' 5
SKETCH COUNT click_counter 'button_a'

-- Cuckoo Filter: membership testing with deletion (unlike Bloom filters)
CREATE FILTER active_sessions CAPACITY 500000
FILTER ADD active_sessions 'session_abc'
FILTER CHECK active_sessions 'session_abc'
FILTER DELETE active_sessions 'session_abc'
```

---

## Advanced Indexes

Beyond B-tree. Create the right index for your workload.

```sql
-- Hash index: O(1) exact-match lookups
CREATE INDEX idx_email ON users (email) USING HASH

-- Bitmap index: fast analytical queries on low-cardinality columns
CREATE INDEX idx_status ON orders (status) USING BITMAP

-- R-Tree: spatial queries on geo data
CREATE INDEX idx_loc ON sites (location) USING RTREE
SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT 50
SEARCH SPATIAL NEAREST 48.8566 2.3522 K 5 COLLECTION sites COLUMN location
```

---

## SQL Extensions

RedDB extends SQL with `WITH` clauses for operational semantics:

```sql
-- TTL: auto-expire records
INSERT INTO sessions (token) VALUES ('abc') WITH TTL 1 h

-- Context indexes for cross-model search
CREATE TABLE customers (passport TEXT) WITH CONTEXT INDEX ON (passport)

-- Graph expansion inline with SELECT
SELECT * FROM users WITH EXPAND GRAPH DEPTH 2

-- Metadata on write
INSERT INTO logs (msg) VALUES ('deploy') WITH METADATA (source = 'ci')

-- Absolute expiration
INSERT INTO events (name) VALUES ('launch') WITH EXPIRES AT 1735689600000
```

---

## 6 Query Languages

Write in whatever you think in. The engine auto-detects the language.

| Language | Example |
|:---------|:--------|
| **SQL** | `SELECT * FROM hosts WHERE os = 'linux'` |
| **Cypher** | `MATCH (a:User)-[:FOLLOWS]->(b) RETURN b.name` |
| **Gremlin** | `g.V().hasLabel('person').out('FOLLOWS').values('name')` |
| **SPARQL** | `SELECT ?name WHERE { ?p :name ?name }` |
| **Natural Language** | `show me all critical hosts` |
| **ASK (RAG)** | `ASK 'what changed in the last 24 hours?'` |

All six hit the same engine, same data, same indexes.

---

## 48 Built-in Types

Not just `TEXT` and `INTEGER`. RedDB understands your domain.

**Network:** `IpAddr`, `Ipv4`, `Ipv6`, `MacAddr`, `Cidr`, `Subnet`, `Port`
**Geo:** `Latitude`, `Longitude`, `GeoPoint`
**Locale:** `Country2`, `Country3`, `Lang2`, `Lang5`, `Currency`
**Identity:** `Uuid`, `Email`, `Url`, `Phone`, `Semver`
**Visual:** `Color`, `ColorAlpha`
**Cross-model refs:** `NodeRef`, `EdgeRef`, `VectorRef`, `RowRef`, `KeyRef`, `DocRef`, `TableRef`, `PageRef`
**Primitives:** `Integer`, `UnsignedInteger`, `Float`, `Decimal`, `BigInt`, `Text`, `Blob`, `Boolean`, `Json`, `Array`, `Enum`
**Temporal:** `Timestamp`, `TimestampMs`, `Date`, `Time`, `Duration`

Validation on write. No parsing in your app.

---

## Backup & Recovery

Built-in backup scheduler, WAL archiving, Change Data Capture (CDC), and Point-in-Time Recovery framework:

```bash
# Poll real-time changes
curl 'localhost:8080/changes?since_lsn=0'

# Trigger manual backup
curl -X POST localhost:8080/backup/trigger

# Check backup status
curl localhost:8080/backup/status
```

Remote backends: S3, R2, DigitalOcean Spaces, GCS, Turso, Cloudflare D1, local filesystem.

---

## KV REST API

Every collection doubles as a key-value store with dedicated REST endpoints:

```bash
# Write a key
curl -X PUT http://127.0.0.1:8080/collections/settings/kvs/theme \
  -H 'content-type: application/json' -d '{"value": "dark"}'

# Read a key
curl http://127.0.0.1:8080/collections/settings/kvs/theme

# Delete a key
curl -X DELETE http://127.0.0.1:8080/collections/settings/kvs/theme
```

Config keys work the same way -- read, write, or delete any `red_config` setting at runtime:

```bash
# Set a config key
curl -X PUT http://127.0.0.1:8080/config/red.ai.default.provider \
  -d '{"value": "groq"}'

# Read a config key
curl http://127.0.0.1:8080/config/red.ai.default.provider

# Or manage config from SQL
SET CONFIG red.ai.default.provider = 'groq'
SHOW CONFIG red.ai
```

---

## 3 Deployment Modes

| Mode | Think of it as... | Access via |
|:-----|:-------------------|:-----------|
| **Embedded** | SQLite | Rust API -- `RedDB::open("data.rdb")` |
| **Server** | Postgres | HTTP + gRPC -- dual-stack |
| **Agent** | MCP tool | `red mcp` -- AI agent integration |

Same storage format across all three. Start embedded, scale to server, expose to agents -- no migration.

---

## Performance

RedDB uses multiple optimization techniques for fast queries at scale:

- **Result Cache** -- identical SELECT queries return in <1ms; auto-invalidated on INSERT/UPDATE/DELETE (30s TTL, max 1000 entries)
- **Hot Entity Cache** -- `get_any(id)` lookups served from an LRU cache (10K entries), O(1) instead of scanning all collections
- **Binary Bulk Insert** -- gRPC `BulkInsertBinary` with zero JSON overhead, protobuf native types -- 241K ops/sec
- **Concurrent HTTP** -- thread-per-connection model; each request handled in its own OS thread
- **Parallel Segment Scanning** -- sealed segments scanned in parallel via `std::thread::scope`; auto-detects single-core and skips parallelism
- **Hash Join** -- O(n+m) joins instead of O(n*m), auto-selected for large datasets
- **Lazy Graph Materialization** -- only loads reachable nodes instead of full graph
- **Pre-filtered Vector Search** -- metadata filters applied before HNSW indexing
- **Index-Assisted Scans** -- bloom filter + hash index hints for WHERE clauses
- **Column Projection Pushdown** -- only materializes SELECT columns
- **Query Plan Caching** -- LRU cache with 1h TTL for repeated queries
- **Batch Entity Lookup** -- multi-entity fetches resolved in a single pass
- **Background Maintenance Thread** -- backup scheduling, retention, and checkpoint run off the hot path

---

## Durability & Corruption Defense

RedDB uses 7 layers of protection to keep your data safe:

| Layer | What it does |
|:------|:-------------|
| **File Lock** | Exclusive `flock` prevents two processes from writing the same `.rdb` file |
| **Double-Write Buffer** | Pages written to `.rdb-dwb` first; survives torn writes on power loss |
| **Header Shadow** | Copy of page 0 in `.rdb-hdr`; auto-recovers if header corrupts |
| **Metadata Shadow** | Copy of page 1 in `.rdb-meta`; auto-recovers collection registry |
| **fsync Discipline** | All critical writes followed by `sync_all()` (not just flush) |
| **Two-Phase Checkpoint** | Crash-safe WAL→DB transfer with `checkpoint_in_progress` flag |
| **Binary Store CRC32** | V3 files have CRC32 footer + atomic write-to-temp-then-rename |

Every page has a CRC32 checksum (verified on read). Every WAL record has a CRC32 checksum. The binary store format (V3) includes a full-file CRC32 footer.

---

## Eventual Consistency

RedDB supports per-field eventual consistency via an append-only transaction log with periodic consolidation. Inspired by CRDT principles (commutative, associative reducers), it enables high-throughput write patterns while guaranteeing convergence.

```bash
# Track clicks with async consolidation (returns instantly)
curl -X POST localhost:8080/ec/urls/clicks/add -d '{"id": 1, "value": 1}'

# Check consolidated + pending value
curl localhost:8080/ec/urls/clicks/status?id=1
```

| Feature | Description |
|:--------|:------------|
| **6 reducers** | Sum, Max, Min, Count, Average, Last (last-write-wins) |
| **Sync mode** | Consolidates immediately (strong consistency) |
| **Async mode** | Background worker consolidates periodically (high throughput) |
| **Transaction log** | Immutable append-only audit trail per field |
| **SET checkpoint** | Resets base value, discards prior operations |
| **All modes** | Works in server, embedded (Rust API), and serverless |

See the [Eventual Consistency Guide](https://forattini-dev.github.io/reddb/#/guides/eventual-consistency) for the theory (CAP theorem, CRDTs, convergence) and full API reference.

---

## Geographic Operations

Built-in geo functions with no external dependencies. Supports both spherical (Haversine) and ellipsoidal (Vincenty/WGS-84) models.

```sql
-- Distance from each store to a point (in km)
SELECT name, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist
FROM stores ORDER BY dist

-- Vincenty for sub-millimeter accuracy
SELECT name, GEO_DISTANCE_VINCENTY(location, POINT(40.71, -74.00)) AS dist
FROM airports
```

```bash
# HTTP API
curl -X POST localhost:8080/geo/distance -d '{
  "from": {"lat": -23.55, "lon": -46.63},
  "to": {"lat": -22.91, "lon": -43.17}
}'
```

| Function | What it computes |
|:---------|:-----------------|
| `GEO_DISTANCE` | Haversine distance (km) |
| `GEO_DISTANCE_VINCENTY` | WGS-84 geodesic distance (km) |
| `GEO_BEARING` | Compass direction (degrees) |
| `GEO_MIDPOINT` | Great-circle midpoint |

Also available: destination point, bounding box, polygon area, spatial search (RADIUS, BBOX, NEAREST). See the [Geo Operations Guide](https://forattini-dev.github.io/reddb/#/guides/geo-operations).

---

## Vector Clustering

Standalone K-Means and DBSCAN clustering on vector collections, with SIMD-accelerated distance computation and automatic parallelization.

```bash
# K-Means: group products into 5 clusters
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "products", "algorithm": "kmeans", "k": 5
}'

# DBSCAN: discover clusters automatically (no K needed)
curl -X POST localhost:8080/vectors/cluster -d '{
  "collection": "products", "algorithm": "dbscan", "eps": 0.5, "min_points": 3
}'
```

K-Means uses parallel assignment (multi-threaded for datasets > 1K vectors). DBSCAN labels unreachable points as noise (-1), useful for outlier detection. See the [Vector Clustering Guide](https://forattini-dev.github.io/reddb/#/guides/vector-clustering).

---

## Native Drivers

One connection-string API, four languages. Every driver accepts the same
`connect(uri)` contract so application code ports across runtimes with zero
ceremony.

| Language          | Package          | Install                        | Backends                            |
|-------------------|------------------|--------------------------------|-------------------------------------|
| Rust              | `reddb-client`   | `cargo add reddb-client`       | embedded ✅ · gRPC ⏳              |
| Node / Bun / Deno | `reddb` (npm)    | `pnpm add reddb`               | stdio subprocess ✅                 |
| Python            | `reddb` (PyPI)   | `pip install reddb` *(soon)*   | embedded ✅ · gRPC ⏳              |

All drivers accept the same URIs:

```
memory://                   ephemeral in-memory
file:///absolute/path       embedded engine on disk
grpc://host:port            remote server (planned — tracked in PLAN_DRIVERS.md)
```

Example — the same app in three languages:

```rust
// Rust
let db = reddb_client::Reddb::connect("memory://").await?;
db.insert("users", &JsonValue::object([("name", JsonValue::string("Alice"))])).await?;
let rows = db.query("SELECT * FROM users").await?;
```

```js
// Node, Bun, Deno
import { connect } from 'reddb'
const db = await connect('memory://')
await db.insert('users', { name: 'Alice' })
const rows = await db.query('SELECT * FROM users')
```

```python
# Python
import reddb
with reddb.connect("memory://") as db:
    db.insert("users", {"name": "Alice"})
    print(db.query("SELECT * FROM users"))
```

Driver docs live in `drivers/rust/README.md`, `drivers/js/README.md`, and
`drivers/python/README.md`. The full protocol spec and roadmap are in
[`PLAN_DRIVERS.md`](./PLAN_DRIVERS.md).

For JavaScript and TypeScript, use the `reddb` package in application code:

```bash
pnpm add reddb
```

```ts
import { connect } from 'reddb'

const db = await connect('memory://')
const result = await db.query('SELECT * FROM users')
await db.close()
```

Use `reddb-cli` only when you want to launch the real `red` binary from npm:

```bash
npx reddb-cli@latest version
npx reddb-cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```

---

## Quick Start

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash

# Start the server
red server --http-bind 127.0.0.1:8080 --path ./data.rdb

# Insert data
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"INSERT INTO hosts (ip, os) VALUES ('\''10.0.0.1'\'', '\''linux'\'')"}'

# Query it
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts"}'
```

Or via npm CLI launcher:

```bash
npx reddb-cli@latest server --http --bind 127.0.0.1:8080
```

---

## Links

- [Documentation](https://forattini-dev.github.io/reddb)
- [GitHub](https://github.com/forattini-dev/reddb)
- [npm driver package](https://www.npmjs.com/package/reddb)
- [npm package](https://www.npmjs.com/package/reddb-cli)
- [Releases](https://github.com/forattini-dev/reddb/releases)

---

**AGPL-3.0 License** -- Built by [Filipe Forattini](https://github.com/forattini-dev)
