# Vectors & Embeddings

RedDB includes a native vector engine for similarity search, supporting HNSW, IVF, product quantization, and hybrid retrieval. Vectors live alongside rows and graph entities in the same database.

## SQL First

If you want to work with vectors from the query language, RedDB exposes search commands directly in SQL-style syntax:

```sql
SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2 MIN_SCORE $3
```

You can also use the canonical vector-query form:

```sql
VECTOR SEARCH embeddings SIMILAR TO [0.12, 0.91, 0.44] LIMIT 5
```

Stored-vector reference form:

```sql
VECTOR SEARCH embeddings SIMILAR TO (embeddings, 42) LIMIT 5
```

Subquery source form:

```sql
VECTOR SEARCH embeddings
SIMILAR TO (VECTOR SEARCH seed_embeddings SIMILAR TO 'remote code execution' LIMIT 1)
LIMIT 5
```

```sql
SEARCH TEXT 'machine learning basics' IN docs LIMIT $1
```

```sql
SEARCH HYBRID TEXT 'neural networks' VECTOR [0.15, 0.89, 0.40] IN docs K $1
```

```sql
SEARCH IVF [0.12, 0.91, 0.44] IN embeddings K 10 PROBES 3
```

Submit placeholder examples through `db.query(sql, params)` so supported
values stay out of the SQL string. `SEARCH SIMILAR` accepts bound vectors,
text, limits, and thresholds; other vector command forms only accept the
placeholder slots implemented by the parser today. The parameterized query
design is tracked in [ADR #352](https://github.com/reddb-io/reddb/issues/352)
until the local ADR lands.

And if you just want to inspect stored vectors:

```sql
FROM ANY
WHERE kind = 'vector' AND collection = 'docs'
LIMIT 20
```

## Inserting Vectors

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:5000/collections/docs/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.91, 0.44, 0.33, 0.67, 0.88, 0.21, 0.55],
    "content": "Introduction to machine learning fundamentals",
    "metadata": {
      "source": "textbook",
      "chapter": 1,
      "topic": "ml-basics"
    }
  }'
```

#### **gRPC**

```bash
grpcurl -plaintext \
  -d '{
    "collection": "docs",
    "payloadJson": "{\"dense\":[0.12,0.91,0.44,0.33,0.67,0.88,0.21,0.55],\"content\":\"Introduction to machine learning\"}"
  }' \
  127.0.0.1:55055 reddb.v1.RedDb/CreateVector
```

#### **Rust (Embedded)**

```rust
let vector_id = db.vector("docs")
    .dense(vec![0.12, 0.91, 0.44, 0.33, 0.67, 0.88, 0.21, 0.55])
    .content("Introduction to machine learning fundamentals")
    .metadata("source", "textbook")
    .save()?;
```

<!-- tabs:end -->

## Vector Fields

| Field | Required | Description |
|:------|:---------|:------------|
| `dense` | Yes | Array of `f32` floats (the embedding) |
| `content` | No | Original text content associated with the embedding |
| `metadata` | No | Key-value pairs for filtering |

## Similarity Search

Find the most similar vectors to a query vector:

```bash
curl -X POST http://127.0.0.1:5000/collections/docs/similar \
  -H 'content-type: application/json' \
  -d '{
    "vector": [0.15, 0.89, 0.40, 0.30, 0.70, 0.85, 0.25, 0.50],
    "k": 5,
    "min_score": 0.7
  }'
```

Parameters:

| Parameter | Type | Default | Description |
|:----------|:-----|:--------|:------------|
| `vector` | `f32[]` | (required) | Query vector |
| `k` | `int` | `10` | Number of results |
| `min_score` | `f32` | `0.0` | Minimum cosine similarity threshold |

SQL form:

```sql
SEARCH SIMILAR $1
COLLECTION docs
LIMIT $2
MIN_SCORE $3
```

Text-to-embedding form:

```sql
SEARCH SIMILAR TEXT $1 COLLECTION docs LIMIT $2
VECTOR SEARCH docs SIMILAR TO 'machine learning fundamentals' LIMIT 5
```

> [!TIP]
> `SEARCH SIMILAR TEXT ... USING provider` lets you choose the embedding provider per query.
> `VECTOR SEARCH ... SIMILAR TO 'text'` uses the runtime default embedding provider and model.
> `VECTOR SEARCH ... SIMILAR TO (<subquery>)` uses the first subquery row and resolves either an inline vector or a vector entity/reference.

## IVF Search

Use inverted file index for approximate search on large datasets:

```bash
curl -X POST http://127.0.0.1:5000/collections/docs/ivf/search \
  -H 'content-type: application/json' \
  -d '{
    "vector": [0.15, 0.89, 0.40, 0.30, 0.70, 0.85, 0.25, 0.50],
    "k": 10,
    "n_probes": 3
  }'
```

SQL form:

```sql
SEARCH IVF [0.15, 0.89, 0.40, 0.30, 0.70, 0.85, 0.25, 0.50]
IN docs
K 10
PROBES 3
```

## Text Search

Full-text search across vector content and metadata:

```bash
curl -X POST http://127.0.0.1:5000/text/search \
  -H 'content-type: application/json' \
  -d '{
    "query": "machine learning basics",
    "collections": ["docs"],
    "limit": 10,
    "fuzzy": true
  }'
```

SQL form:

```sql
SEARCH TEXT 'machine learning basics' IN docs LIMIT $1
```

```sql
SEARCH TEXT 'machne lerning' IN docs FUZZY LIMIT $1
```

## Hybrid Search

Combine structured filters with vector similarity:

```bash
curl -X POST http://127.0.0.1:5000/hybrid/search \
  -H 'content-type: application/json' \
  -d '{
    "collections": ["docs"],
    "vector": [0.15, 0.89, 0.40, 0.30, 0.70, 0.85, 0.25, 0.50],
    "query": "neural networks",
    "k": 10,
    "filters": {
      "topic": "ml-basics"
    }
  }'
```

SQL form:

```sql
SEARCH HYBRID TEXT 'neural networks' VECTOR [0.15, 0.89, 0.40] IN docs K $1
```

## Inspecting Stored Vectors with SQL

Use universal queries when you want the vector entities themselves instead of a search result:

```sql
FROM ANY
WHERE kind = 'vector' AND collection = 'docs'
ORDER BY rid DESC
LIMIT 20
```

This is useful for debugging ingestion pipelines, checking metadata, or auditing collections.

## Bulk Insert

```bash
curl -X POST http://127.0.0.1:5000/collections/docs/bulk/vectors \
  -H 'content-type: application/json' \
  -d '[
    {"dense": [0.1, 0.2, 0.3], "content": "Document A"},
    {"dense": [0.4, 0.5, 0.6], "content": "Document B"},
    {"dense": [0.7, 0.8, 0.9], "content": "Document C"}
  ]'
```

## AUTO EMBED

`WITH AUTO EMBED` lets row inserts create linked vectors without a separate embedding call:

```sql
INSERT INTO docs (id, title, body)
VALUES
  ($1, $2, $3),
  ($4, $5, $6)
WITH AUTO EMBED (title, body) USING openai MODEL 'text-embedding-3-small'
```

For bulk inserts, RedDB batches all non-empty embedding texts through `AiBatchClient`, applies provider retry/timeout handling, and then stores one vector per embedded row. The SQL and HTTP request shape did not change; batching is transparent to clients.

## Auto-embed over CDC

Instead of repeating `WITH AUTO EMBED` on every insert, a collection can declare
an `EMBED` policy once in its DDL. Then **every** write to that collection is
embedded automatically, asynchronously, off the write path:

```sql
CREATE TABLE articles (id INT, title TEXT, body TEXT)
WITH (
  EMBED (fields = ('title', 'body'), provider = 'openai', model = 'text-embedding-3-small')
)
```

With an `EMBED` policy in place:

- An `INSERT`/`UPDATE` commits and returns immediately — it emits its usual CDC
  change event and does **no** provider work, so write latency stays independent
  of the embedding provider.
- A CDC enrichment consumer later drains the change stream, embeds the declared
  fields, and attaches the vector — exactly as a manual `WITH AUTO EMBED` insert
  would have.
- Until the vector is attached, the row is **`pending`** and is naturally
  excluded from `VECTOR SEARCH` (it has no vector yet), so a search never returns
  a half-enriched set. Failed enrichment retries with backoff and then
  dead-letters, with an operator re-drive path.

> [!TIP]
> Inline `WITH AUTO EMBED` (above) and the declarative `EMBED` policy attach
> vectors the same way and are searchable identically. Use inline `AUTO EMBED`
> for one-off control per insert; use the `EMBED` policy when you want every
> write to a collection embedded without restating it.

See [Per-collection AI policy](../query/ai-policy.md) for the full `EMBED`
grammar, the retry/dead-letter/re-drive states, and DDL-time provider
validation.

## Distance Metrics

| Metric | Description |
|:-------|:------------|
| Cosine | Cosine similarity (default) |
| Euclidean | L2 distance |
| Dot Product | Inner product |

## Index Types

RedDB supports multiple vector index strategies:

| Index | Best For | Trade-off |
|:------|:---------|:----------|
| **Flat** | Small datasets (< 10K vectors) | Exact results, O(n) search |
| **HNSW** | Medium datasets | Fast approximate search, higher memory |
| **IVF** | Large datasets | K-means clustering, tunable probes |
| **PQ** | Very large datasets | Compressed vectors, lower memory |

See [Vector Engine](/vectors/hnsw.md) for detailed index configuration.

> [!TIP]
> Vectors participate in `FROM ANY` universal queries. You can combine vector similarity with table filters and graph traversals in a single query.
