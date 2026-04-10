<p align="center">
  <h1 align="center">RedDB</h1>
  <p align="center"><strong>The AI-first multi-model database.</strong></p>
  <p align="center">Tables. Documents. Graphs. Vectors. KV. One engine. Ask it anything.</p>
</p>

<p align="center">
  <a href="https://github.com/forattini-dev/reddb/releases"><img src="https://img.shields.io/github/v/release/forattini-dev/reddb?style=flat-square" alt="Release"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License"></a>
  <a href="https://www.npmjs.com/package/reddb-cli"><img src="https://img.shields.io/npm/v/reddb-cli?style=flat-square&label=npm" alt="npm"></a>
</p>

---

## The Killer Feature: `ASK`

```sql
ASK 'who owns CPF 000.000.000-00 and what services do they use?'
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
CREATE TABLE customers (cpf TEXT) WITH CONTEXT INDEX ON (cpf)

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

## 3 Deployment Modes

| Mode | Think of it as... | Access via |
|:-----|:-------------------|:-----------|
| **Embedded** | SQLite | Rust API -- `RedDB::open("data.rdb")` |
| **Server** | Postgres | HTTP + gRPC -- dual-stack |
| **Agent** | MCP tool | `red mcp` -- AI agent integration |

Same storage format across all three. Start embedded, scale to server, expose to agents -- no migration.

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

Or via npm:

```bash
npx reddb-cli@latest server --http --bind 127.0.0.1:8080
```

---

## Links

- [Documentation](https://forattini-dev.github.io/reddb)
- [GitHub](https://github.com/forattini-dev/reddb)
- [npm package](https://www.npmjs.com/package/reddb-cli)
- [Releases](https://github.com/forattini-dev/reddb/releases)

---

**MIT License** -- Built by [Filipe Forattini](https://github.com/forattini-dev)
