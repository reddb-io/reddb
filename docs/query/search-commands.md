# Search Commands

RedDB supports vector, text, hybrid, multimodal, and indexed lookup commands within the query engine.

## SEARCH SIMILAR

Find vectors similar to a query vector:

```sql
SEARCH SIMILAR [0.12, 0.91, 0.44] IN embeddings K 5 MIN_SCORE 0.7
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

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `TEXT 'query'` | Yes | Natural-language query to embed |
| `COLLECTION col` | Yes | Collection to search |
| `LIMIT n` | No | Number of results (default 10) |
| `USING provider` | No | Embedding provider (`openai`, `groq`, `ollama`, `anthropic`) |

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
SEARCH MULTIMODAL 'CPF: 081.232.036-08' COLLECTION people LIMIT 20
```

## SEARCH INDEX

Lookup estruturado por índice global:

```sql
SEARCH INDEX cpf VALUE '081.232.036-08' COLLECTION people LIMIT 20
```

Por padrão, o lookup é exato. Para modo mais flexível:

```sql
SEARCH INDEX cpf VALUE '081.232.036-08' FUZZY LIMIT 20
```

## SEARCH CONTEXT

Unified context search across **all** data structures — tables, graphs, vectors, documents, and key-values — in a single command. SEARCH CONTEXT uses a 3-tier strategy (field-value index, token index, then global scan), automatically expands results via graph traversal and cross-references, groups results by structure type, and returns connections between found entities.

```sql
SEARCH CONTEXT '<query>' [FIELD <field>] [COLLECTION <col>] [DEPTH <n>] [LIMIT <n>]
```

### Parameters

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `query` | Yes | Search term — CPF, IP, name, ID, or any value |
| `FIELD field` | No | Target a specific field for indexed lookup |
| `COLLECTION col` | No | Scope the search to a specific collection |
| `DEPTH n` | No | Graph traversal depth (default 1, max 3) |
| `LIMIT n` | No | Maximum results per structure type (default 25) |

### Examples

Search across everything with a CPF:

```sql
SEARCH CONTEXT '081.232.036-08'
```

Narrow to a specific indexed field:

```sql
SEARCH CONTEXT '081.232.036-08' FIELD cpf
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
| `USING provider` | No | LLM provider (`openai`, `groq`, `ollama`, `anthropic`) |
| `MODEL 'name'` | No | Specific model to use (provider-dependent) |

> [!TIP]
> `ASK` performs a context search behind the scenes, so it benefits from the same indexes and graph traversals used by `SEARCH CONTEXT`.

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
    "query": "CPF: 081.232.036-08",
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
      "index": "cpf",
      "value": "081.232.036-08",
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
    "query": "081.232.036-08",
    "field": "cpf"
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
