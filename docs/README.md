# RedDB

> The AI-first multi-model database. Ask questions in plain language, search semantically, and query tables, documents, graphs, vectors, and key-value data from one engine.

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

## Five data models, one runtime

You do not need a SQL store, a document store, a graph database, a vector database, and a KV cache. RedDB keeps all five in one process.

| Model | Use case | Example query |
|:------|:---------|:--------------|
| Tables & Rows | Structured records, typed columns | `SELECT * FROM users WHERE active = true` |
| Documents | Schema-free payloads, nested fields | `SELECT * FROM logs WHERE payload.level = 'error'` |
| Key-Value | Config, sessions, feature flags | `GET config/max_retries` |
| Graphs | Relationships, traversals, analytics | `GRAPH SHORTEST_PATH FROM 'A' TO 'Z' IN network` |
| Vectors | Embeddings, semantic search, RAG | `SEARCH SIMILAR [0.12, 0.91, 0.44] IN embeddings K 5` |

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

## Run anywhere

The same engine runs embedded in your Rust binary, as an HTTP/gRPC server, or as an MCP tool server for AI agents.

| Mode | Best for | Access path |
|:-----|:---------|:------------|
| Embedded | Local-first apps, CLIs, edge binaries | `RedDB::open("./data.rdb")` in Rust |
| Server | Shared databases, service-to-service traffic | HTTP or gRPC |
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
