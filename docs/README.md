# RedDB

> The AI-first multi-model database. Ask questions in plain language, search semantically, and query tables, documents, graphs, vectors, and key-value data from one engine.

> [!IMPORTANT]
> In RedDB, a `collection` is the named logical container for data. Tables, documents, KV, graphs, vectors,
> time-series, and queues are the data-model semantics you use on top of collections. A collection does not
> sit above tables and documents as a separate hierarchy layer. Instead, `users` can be a collection used as
> a table, `events` can be a collection used for documents, and `config` can be a collection used as KV.
> Some models can also coexist in the same collection when that fits the workload.

## Ask your database anything

RedDB has a built-in AI layer. You write a question, RedDB searches for relevant context across every collection, sends it to an LLM, and returns a synthesized answer. No external pipelines. No glue code.

```sql
ASK 'what happened on host 10.0.0.1 in the last 24 hours?' USING groq

ASK 'summarize all critical vulnerabilities' USING anthropic MODEL 'claude-sonnet-4-20250514'

ASK 'list users with admin access and their login history' USING ollama MODEL 'llama3'
```

`ASK` works over every data model: it pulls rows, documents, graph paths, and vector matches into a single context window before the LLM answers.

## 11 AI providers, zero configuration

Set an API key and go. Every provider that exposes an OpenAI-compatible API works out of the box.

| Provider | Token | Embedding | Prompt |
|:---------|:------|:----------|:-------|
| OpenAI | `openai` | yes | yes |
| Anthropic | `anthropic` | -- | yes |
| Groq | `groq` | yes | yes |
| OpenRouter | `openrouter` | yes | yes |
| Together | `together` | yes | yes |
| Venice | `venice` | yes | yes |
| DeepSeek | `deepseek` | yes | yes |
| Ollama | `ollama` | yes | yes |
| HuggingFace | `huggingface` | yes | yes |
| Local | `local` | stub | stub |
| Custom URL | *(url)* | yes | yes |

```bash
export REDDB_GROQ_API_KEY=gsk_xxx
red server --path ./data/reddb.rdb --http-bind 127.0.0.1:8080
```

## Semantic search built in

Search by meaning, not just keywords. Pass a text string and RedDB generates the embedding, runs the similarity search, and returns ranked results.

```sql
SEARCH SIMILAR TEXT 'suspicious login attempt' COLLECTION logs LIMIT 10

SEARCH SIMILAR TEXT 'CVE in OpenSSH' COLLECTION cves LIMIT 5 USING openai
```

Auto-embed on insert so every record is searchable the moment it lands:

```sql
INSERT INTO incidents (title, body) VALUES ('SSH brute force', 'Detected 500 failed attempts...')
  WITH AUTO EMBED (body) USING openai
```

## Seven user-facing data models, one runtime

You do not need a SQL store, a document store, a graph database, a vector database, a KV cache,
plus separate time-series and queue infrastructure. RedDB keeps all seven user-facing models in one
process.

The key naming rule is simple: the model is how you work with the data, and the collection is where
that data lives.

| Model | Use case | Example query |
|:------|:---------|:--------------|
| Tables & Rows | Structured records, typed columns | `SELECT * FROM users WHERE active = true` |
| Documents | Schema-free payloads, nested fields | `SELECT * FROM logs WHERE payload.level = 'error'` |
| Key-Value | Config, sessions, feature flags | `GET config/max_retries` |
| Graphs | Relationships, traversals, analytics | `GRAPH SHORTEST_PATH FROM 'A' TO 'Z' IN network` |
| Vectors | Embeddings, semantic search, RAG | `SEARCH SIMILAR [0.12, 0.91, 0.44] IN embeddings K 5` |
| Time-Series | Metrics, telemetry, bucketed aggregations | `SELECT avg(value) FROM cpu_metrics GROUP BY time_bucket(5m)` |
| Queues | Jobs, retries, consumer groups, DLQ | `QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 5` |

For the distinction between user-facing models, native persisted entity kinds, and supporting
structures such as indexes and probabilistic sketches, see
[Data Model Overview](/data-models/overview.md).

## Six query languages

Write queries in the style you already know. RedDB parses RQL, SQL, Gremlin, SPARQL, Cypher, and natural language.

```sql
-- RQL / SQL
SELECT name, email FROM users WHERE active = true LIMIT 10

-- Gremlin
g.V().hasLabel('user').has('active', true).values('name')

-- Natural language
FIND users who logged in this week
```

## Native time-series and queues

Time-series and queues are not just naming conventions. They are dedicated models with their own
runtime semantics and persistence paths.

```sql
-- Time-series with retention and compression
CREATE TIMESERIES cpu_metrics RETENTION 90 d
INSERT INTO cpu_metrics (metric, value, tags) VALUES ('cpu.idle', 95.2, {host: 'srv1'})

-- Message queues with priority and consumer groups
CREATE QUEUE tasks PRIORITY MAX_SIZE 10000
QUEUE PUSH tasks {job: 'process', id: 123} PRIORITY 10
QUEUE POP tasks
```

## Probabilistic data structures

Built-in HyperLogLog, Count-Min Sketch, and Cuckoo Filter:

```sql
CREATE HLL visitors
HLL ADD visitors 'user1' 'user2' 'user3'
HLL COUNT visitors              -- ~3, using only 16KB of memory

CREATE SKETCH clicks WIDTH 2000 DEPTH 7
SKETCH ADD clicks 'signup_btn' 5
SKETCH COUNT clicks 'signup_btn' -- ~5

CREATE FILTER sessions CAPACITY 500000
FILTER ADD sessions 'sess_abc'
FILTER CHECK sessions 'sess_abc' -- true
FILTER DELETE sessions 'sess_abc'
```

## Advanced indexes

Hash, Bitmap, R-Tree, and Bloom filter -- the optimizer picks the right one:

```sql
CREATE INDEX idx_email ON users (email) USING HASH       -- O(1) exact match
CREATE INDEX idx_status ON orders (status) USING BITMAP   -- instant COUNT/GROUP BY
CREATE INDEX idx_loc ON sites (location) USING RTREE      -- geo queries

SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location
SEARCH SPATIAL NEAREST 48.8566 2.3522 K 5 COLLECTION sites COLUMN location
```

## JSON inline (no quotes needed)

```sql
INSERT INTO logs (data) VALUES ({level: 'info', msg: 'deploy complete', meta: {env: 'prod'}})
QUEUE PUSH tasks {job: 'email', to: 'alice@co.com', template: 'welcome'}
```

## PostgreSQL-compatible

Full transactions with savepoints, row-level security, declarative
multi-tenancy, views, partitioning, and foreign data wrappers. Speaks
the PostgreSQL wire protocol so existing clients (psql, pgAdmin,
JDBC, pgx, psycopg) connect without changes.

```sql
-- transactions with savepoints
BEGIN;
INSERT INTO users (id, email) VALUES (1, 'a@b');
SAVEPOINT try;
DELETE FROM users WHERE id = 1;
ROLLBACK TO SAVEPOINT try;   -- user 1 survives
COMMIT;

-- declarative multi-tenancy
CREATE TABLE documents (id INT, content TEXT, tenant_id TEXT)
  TENANT BY (tenant_id);
SET TENANT 'acme';
SELECT * FROM documents;     -- automatically scoped to 'acme'

-- row-level security
CREATE POLICY own_rows ON orders USING (customer_id = CURRENT_USER());
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
```

See [Transactions](/query/transactions.md), [Multi-Tenancy](/security/multi-tenancy.md),
[Row Level Security](/security/rls.md), and [PostgreSQL Wire](/api/postgres-wire.md).

## Run anywhere

The same engine runs embedded in your Rust binary, as an HTTP/gRPC server, or as an MCP tool server for AI agents.

If the terms `binary`, `embedded`, `gRPC`, `wire`, and `router` feel overloaded, read [Modes and Transports](/getting-started/modes-and-transports.md) first. That page defines the vocabulary used in the rest of the docs.

| Mode | Best for | Access path |
|:-----|:---------|:------------|
| Embedded | Local-first apps, CLIs, edge binaries | `RedDB::open("./data.rdb")` in Rust |
| Server | Shared databases, service-to-service traffic | Router, HTTP, gRPC, or wire |
| Agent tooling | AI workflows, MCP-compatible agents | CLI or MCP server |

## Install

### GitHub Releases

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash
```

Pin a version:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --version v0.1.2
```

### npx

```bash
npx reddb-cli@latest server --path ./data/reddb.rdb --http-bind 127.0.0.1:8080
```

### JavaScript / TypeScript driver

```bash
pnpm add reddb
```

```ts
import { connect } from 'reddb'

const db = await connect('memory://')
const result = await db.query('SELECT * FROM hosts')
await db.close()
```

## First connection

Start the server:

```bash
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080
```

Check health:

```bash
curl -s http://127.0.0.1:8080/health
```

Run a query:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts"}'
```

Ask a question:

```bash
curl -X POST http://127.0.0.1:8080/ai/ask \
  -H 'content-type: application/json' \
  -d '{"question":"what happened on host 10.0.0.1?","provider":"groq","model":"llama-3.3-70b-versatile"}'
```

Connect via gRPC REPL:

```bash
red connect 127.0.0.1:50051
```

## Start here

<div class="grid-3">
  <a href="#/getting-started/installation" class="card">
    <h4>Installation</h4>
    <p>Install from GitHub Releases, npx, or source.</p>
  </a>
  <a href="#/query/search-commands" class="card">
    <h4>ASK & Search</h4>
    <p>Ask questions, semantic search, and context retrieval.</p>
  </a>
  <a href="#/api/http" class="card">
    <h4>AI Providers</h4>
    <p>Configure OpenAI, Groq, Anthropic, Ollama, and more.</p>
  </a>
</div>
