# Vectors & Embeddings

RedDB includes a native vector engine for similarity search, supporting HNSW, IVF, product quantization, and hybrid retrieval. Vectors live alongside rows and graph entities in the same database.

## SQL First

If you want to work with vectors from the query language, RedDB exposes search commands directly in SQL-style syntax:

```sql
SEARCH SIMILAR [0.12, 0.91, 0.44] IN embeddings K 5 MIN_SCORE 0.7
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
SEARCH TEXT 'machine learning basics' IN docs LIMIT 10
```

```sql
SEARCH HYBRID TEXT 'neural networks' VECTOR [0.15, 0.89, 0.40] IN docs K 10
```

```sql
SEARCH IVF [0.12, 0.91, 0.44] IN embeddings K 10 PROBES 3
```

And if you just want to inspect stored vectors:

```sql
FROM ANY
WHERE _kind = 'vector' AND _collection = 'docs'
LIMIT 20
```

## Inserting Vectors

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:8080/collections/docs/vectors \
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
  127.0.0.1:50051 reddb.v1.RedDb/CreateVector
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
curl -X POST http://127.0.0.1:8080/collections/docs/similar \
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
SEARCH SIMILAR [0.15, 0.89, 0.40, 0.30, 0.70, 0.85, 0.25, 0.50]
IN docs
K 5
MIN_SCORE 0.7
```

Text-to-embedding form:

```sql
SEARCH SIMILAR TEXT 'machine learning fundamentals' COLLECTION docs LIMIT 5
VECTOR SEARCH docs SIMILAR TO 'machine learning fundamentals' LIMIT 5
```

> [!TIP]
> `SEARCH SIMILAR TEXT ... USING provider` lets you choose the embedding provider per query.
> `VECTOR SEARCH ... SIMILAR TO 'text'` uses the runtime default embedding provider and model.
> `VECTOR SEARCH ... SIMILAR TO (<subquery>)` uses the first subquery row and resolves either an inline vector or a vector entity/reference.

## IVF Search

Use inverted file index for approximate search on large datasets:

```bash
curl -X POST http://127.0.0.1:8080/collections/docs/ivf/search \
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
curl -X POST http://127.0.0.1:8080/text/search \
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
SEARCH TEXT 'machine learning basics' IN docs LIMIT 10
```

```sql
SEARCH TEXT 'machne lerning' IN docs FUZZY LIMIT 10
```

## Hybrid Search

Combine structured filters with vector similarity:

```bash
curl -X POST http://127.0.0.1:8080/hybrid/search \
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
SEARCH HYBRID TEXT 'neural networks' VECTOR [0.15, 0.89, 0.40] IN docs K 10
```

## Inspecting Stored Vectors with SQL

Use universal queries when you want the vector entities themselves instead of a search result:

```sql
FROM ANY
WHERE _kind = 'vector' AND _collection = 'docs'
ORDER BY _entity_id DESC
LIMIT 20
```

This is useful for debugging ingestion pipelines, checking metadata, or auditing collections.

## Bulk Insert

```bash
curl -X POST http://127.0.0.1:8080/collections/docs/bulk/vectors \
  -H 'content-type: application/json' \
  -d '[
    {"dense": [0.1, 0.2, 0.3], "content": "Document A"},
    {"dense": [0.4, 0.5, 0.6], "content": "Document B"},
    {"dense": [0.7, 0.8, 0.9], "content": "Document C"}
  ]'
```

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
