# Search Commands

RedDB supports vector, text, hybrid, multimodal, and indexed lookup commands within the query engine.

## SEARCH SIMILAR

Find vectors similar to a query vector:

```sql
SEARCH SIMILAR [0.12, 0.91, 0.44] IN embeddings K 5 MIN_SCORE 0.7
```

If you are using the canonical vector-query form inside `/query`, the equivalent syntax is:

```sql
VECTOR SEARCH embeddings SIMILAR TO [0.12, 0.91, 0.44] LIMIT 5
```

You can also reuse a stored vector directly as the query source:

```sql
VECTOR SEARCH embeddings SIMILAR TO (embeddings, 42) LIMIT 5
```

Or derive the query vector from another query. The first subquery row is used, and it must resolve to a vector value, vector reference, or vector entity id:

```sql
VECTOR SEARCH embeddings
SIMILAR TO (VECTOR SEARCH seed_embeddings SIMILAR TO 'remote code execution' LIMIT 1)
LIMIT 5
```

### Parameters

| Parameter | Required | Description |
|:----------|:---------|:------------|
| Vector | Yes | Query vector as array of floats |
| `IN collection` | Yes | Collection to search |
| `K n` | No | Number of results (default 10) |
| `MIN_SCORE f` | No | Minimum similarity threshold |

### Semantic Search (Text-Based Similarity)

Instead of providing a raw vector, you can pass a text string. RedDB generates the embedding automatically and runs the similarity search against the target collection.

```sql
-- Search by text (embedding generated automatically)
SEARCH SIMILAR TEXT 'suspicious login attempt' COLLECTION logs LIMIT 10
SEARCH SIMILAR TEXT 'CVE in OpenSSH' COLLECTION cves LIMIT 5 USING openai

-- Specify provider
SEARCH SIMILAR TEXT 'anomaly detected' COLLECTION events USING groq
SEARCH SIMILAR TEXT 'network scan' COLLECTION logs USING ollama
```

The same semantic lookup is also available in vector-query form:

```sql
VECTOR SEARCH logs SIMILAR TO 'suspicious login attempt' LIMIT 10
VECTOR SEARCH cves SIMILAR TO 'remote code execution' LIMIT 5
```

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `TEXT 'query'` | Yes | Natural-language query to embed |
| `COLLECTION col` | Yes | Collection to search |
| `LIMIT n` | No | Number of results (default 10) |
| `USING provider` | No | Embedding provider (`openai`, `groq`, `ollama`, `anthropic`) |

> [!TIP]
> `VECTOR SEARCH ... SIMILAR TO 'text'` uses the runtime's configured default embedding provider
> and model. Use `SEARCH SIMILAR TEXT ... USING provider` when you want to choose the provider
> explicitly in the query itself.

## SEARCH TEXT

Full-text search across collections:

```sql
SEARCH TEXT 'machine learning basics' IN docs LIMIT 10
```

### With Fuzzy Matching

```sql
SEARCH TEXT 'machne lerning' IN docs FUZZY LIMIT 10
```

## SEARCH HYBRID

Combine text and vector search:

```sql
SEARCH HYBRID TEXT 'neural networks' VECTOR [0.15, 0.89, 0.40] IN docs K 10
```

## SEARCH IVF

Use the IVF index for approximate search on large collections:

```sql
SEARCH IVF [0.12, 0.91, 0.44] IN embeddings K 10 PROBES 3
```

## SEARCH MULTIMODAL

Lookup global por chave em tabelas, documentos, key-values, vetores e grafos:

```sql
SEARCH MULTIMODAL 'passport: AB1234567' COLLECTION people LIMIT 20
```

## SEARCH INDEX

Lookup estruturado por índice global:

```sql
SEARCH INDEX passport VALUE 'AB1234567' COLLECTION people LIMIT 20
```

Por padrão, o lookup é exato. Para modo mais flexível:

```sql
SEARCH INDEX passport VALUE 'AB1234567' FUZZY LIMIT 20
```

## SEARCH CONTEXT

Unified context search across **all** data structures — tables, graphs, vectors, documents, and key-values — in a single command. SEARCH CONTEXT uses a 3-tier strategy (field-value index, token index, then global scan), automatically expands results via graph traversal and cross-references, groups results by structure type, and returns connections between found entities.

```sql
SEARCH CONTEXT '<query>' [FIELD <field>] [COLLECTION <col>] [DEPTH <n>] [LIMIT <n>]
```

### Parameters

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `query` | Yes | Search term — passport, IP, name, ID, or any value |
| `FIELD field` | No | Target a specific field for indexed lookup |
| `COLLECTION col` | No | Scope the search to a specific collection |
| `DEPTH n` | No | Graph traversal depth (default 1, max 3) |
| `LIMIT n` | No | Maximum results per structure type (default 25) |

### Examples

Search across everything with a passport:

```sql
SEARCH CONTEXT 'AB1234567'
```

Narrow to a specific indexed field:

```sql
SEARCH CONTEXT 'AB1234567' FIELD passport
```

Scope to a collection with deeper graph expansion:

```sql
SEARCH CONTEXT 'Alice' COLLECTION customers DEPTH 2 LIMIT 50
```

### Response Shape

Results are grouped by structure type. The response includes a `connections` list linking related entities and a `summary` with hit counts:

```json
{
  "ok": true,
  "tables": [ /* matching table rows */ ],
  "graph": {
    "nodes": [ /* matching graph nodes */ ],
    "edges": [ /* edges connecting found nodes */ ]
  },
  "vectors": [ /* matching vector entries */ ],
  "documents": [ /* matching documents */ ],
  "key_values": [ /* matching key-value pairs */ ],
  "connections": [
    { "from": "entity:42", "to": "entity:17", "rel": "KNOWS" }
  ],
  "summary": {
    "total_hits": 12,
    "tables": 3,
    "graph_nodes": 4,
    "graph_edges": 2,
    "vectors": 1,
    "documents": 1,
    "key_values": 1
  }
}
```

### Response Structure

Each field in the response carries a specific role:

| Field | Type | Description |
|:------|:-----|:------------|
| `tables` | array | Table rows that matched the query. Each entry contains the row fields, entity ID, collection name, relevance score, and how it was discovered (indexed lookup, global scan, or graph traversal). |
| `graph.nodes` | array | Graph nodes found during the search or added during graph expansion. Includes node label, type, properties, and the collection they belong to. |
| `graph.edges` | array | Edges that connect found nodes. Each edge carries its label (e.g. `REPORTS_TO`), source and target node IDs, weight, and any edge properties. |
| `vectors` | array | Vector entries whose content or metadata matched the query. Includes the similarity score when discovered via vector search. |
| `documents` | array | Document entities that matched by content or metadata fields. |
| `key_values` | array | Key-value pairs where the key or value matched the search term. |
| `connections` | array | Links between found entities across structures. Each connection has a `from_id`, `to_id`, a `connection_type` (`CrossRef`, `GraphEdge`, or `VectorSimilarity`), and a `weight` indicating relevance. |
| `summary` | object | Aggregated hit counts and execution metadata. Contains `total_entities`, `direct_matches`, `expanded_via_graph`, `expanded_via_cross_refs`, `expanded_via_vector_query`, `collections_searched`, `execution_time_us`, `tiers_used` (which search tiers fired: `index`, `token`, `scan`), and `entities_reindexed`. |

> [!TIP]
> The `summary.tiers_used` array tells you which search strategies contributed results. A result found via `index` is the fastest path; `scan` means RedDB fell through to a global scan. Use `FIELD` to target an indexed field and avoid full scans on large datasets.

## ASK

Ask natural-language questions against your data. RedDB retrieves relevant context from all collections and generates an answer using the configured LLM provider.

```sql
ASK 'what happened on host 10.0.0.1?' USING groq
ASK 'summarize all vulnerabilities' USING anthropic MODEL 'claude-sonnet-4-20250514'
ASK 'list all users with admin access' USING ollama MODEL 'llama3'
```

### Parameters

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `'question'` | Yes | Natural-language question |
| `USING provider` | No | LLM provider (`openai`, `groq`, `ollama`, `anthropic`, etc.) |
| `MODEL 'name'` | No | Specific model to use (provider-dependent) |
| `DEPTH n` | No | Graph traversal depth for context retrieval (default from config) |
| `LIMIT n` | No | Maximum context results per structure type |
| `COLLECTION col` | No | Scope context retrieval to a specific collection |

### Examples

```sql
-- Basic question using a specific provider
ASK 'who owns passport AB1234567?' USING groq

-- Specify model and depth
ASK 'summarize all vulnerabilities' USING anthropic MODEL 'claude-sonnet-4-20250514' DEPTH 2

-- Scope to a collection with a result limit
ASK 'what changed today?' COLLECTION audit_logs LIMIT 50

-- All optional clauses combined
ASK 'explain the network topology' USING ollama MODEL 'llama3' DEPTH 3 LIMIT 100 COLLECTION network
```

> [!TIP]
> `ASK` performs a context search behind the scenes, so it benefits from the same indexes and graph traversals used by `SEARCH CONTEXT`.

### How ASK Works

ASK executes a three-phase pipeline:

1. **Search Context** — Runs `SEARCH CONTEXT` with your question as the query. Finds all related entities across tables, graphs, vectors, documents, and key-values.

2. **Build LLM Context** — Serializes the search results into a structured prompt. Includes:
   - Database schema (collection names and entity counts)
   - Matched entities grouped by type
   - Graph edges and cross-references between found entities

3. **LLM Synthesis** — Sends the context + your question to the configured AI provider. The LLM generates a natural language answer citing which collections and entities its answer is based on.

If no provider is configured, ASK returns an error. Configure one with:

```bash
curl -X POST http://127.0.0.1:8080/ai/credentials \
  -H 'content-type: application/json' \
  -d '{"provider":"groq","api_key":"gsk_xxx","default":true}'
```

> [!NOTE]
> ASK always searches ALL collections unless you specify `COLLECTION`. To limit scope:
> ```sql
> ASK 'question' COLLECTION incidents DEPTH 1 LIMIT 10
> ```

## SET CONFIG / SHOW CONFIG

Manage runtime configuration directly from the query engine. Changes take effect immediately without restart.

### SET CONFIG

```sql
-- Set a config value
SET CONFIG red.ai.default.provider = 'groq'
SET CONFIG red.ai.default.model = 'llama-3.3-70b-versatile'
SET CONFIG red.storage.hnsw.ef_search = 100
SET CONFIG red.search.rag.graph_depth = 3
SET CONFIG red.backup.enabled = true
```

### SHOW CONFIG

```sql
-- Show all config
SHOW CONFIG

-- Show config subtree
SHOW CONFIG red.ai
SHOW CONFIG red.storage
SHOW CONFIG red.backup
```

### Parameters

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `key` | Yes (SET) | Dot-notation config key (e.g. `red.ai.default.provider`) |
| `value` | Yes (SET) | Value to set (string, integer, float, or boolean) |
| `prefix` | No (SHOW) | Filter by key prefix; omit to show all keys |

> [!TIP]
> These commands are equivalent to the `GET /config/{key}`, `PUT /config/{key}`, and `GET /config` HTTP endpoints. See [Configuration](/getting-started/configuration.md) for the full list of available keys.

## Via HTTP

### Similarity Search

```bash
curl -X POST http://127.0.0.1:8080/collections/docs/similar \
  -H 'content-type: application/json' \
  -d '{
    "vector": [0.12, 0.91, 0.44],
    "k": 5,
    "min_score": 0.7
  }'
```

### Text Search

```bash
curl -X POST http://127.0.0.1:8080/text/search \
  -H 'content-type: application/json' \
  -d '{
    "query": "machine learning",
    "collections": ["docs"],
    "limit": 10,
    "fuzzy": true
  }'
```

### Hybrid Search

```bash
curl -X POST http://127.0.0.1:8080/hybrid/search \
  -H 'content-type: application/json' \
  -d '{
    "collections": ["docs"],
    "vector": [0.12, 0.91, 0.44],
    "query": "neural networks",
    "k": 10
  }'
```

### Multimodal Search

```bash
curl -X POST http://127.0.0.1:8080/multimodal/search \
  -H 'content-type: application/json' \
  -d '{
    "query": "passport: AB1234567",
    "collections": ["people", "documents", "graph", "vectors"],
    "limit": 20
  }'
```

You can also send `"key"` instead of `"query"` in the payload.

### Unified Search (single box)

```bash
curl -X POST http://127.0.0.1:8080/search \
  -H 'content-type: application/json' \
  -d '{
    "mode": "index",
    "lookup": {
      "index": "passport",
      "value": "AB1234567",
      "exact": true
    },
    "limit": 20
  }'
```

`mode` aceita `auto`, `index`, `multimodal` ou `hybrid`.

### Context Search

```bash
curl -X POST http://127.0.0.1:8080/context \
  -H 'content-type: application/json' \
  -d '{
    "query": "AB1234567",
    "field": "passport"
  }'
```

You can also pass `collection`, `depth`, and `limit` in the JSON body.

## Response Format

Search results include similarity scores:

```json
{
  "ok": true,
  "results": [
    {
      "_entity_id": 42,
      "_collection": "docs",
      "_kind": "vector",
      "_score": 0.956,
      "content": "Introduction to neural network architectures",
      "metadata": {"topic": "deep-learning"}
    },
    {
      "_entity_id": 17,
      "_collection": "docs",
      "_kind": "vector",
      "_score": 0.891,
      "content": "Machine learning fundamentals and applications"
    }
  ]
}
```

> [!TIP]
> Vector search works across all vector index types (Flat, HNSW, IVF, PQ). The engine automatically selects the best available index.
