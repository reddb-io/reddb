# Search Commands

RedDB supports vector, text, and hybrid search commands within the query engine.

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

## Via HTTP

### Similarity Search

```bash
curl -X POST http://127.0.0.1:8080/search/similar \
  -H 'content-type: application/json' \
  -d '{
    "collection": "docs",
    "vector": [0.12, 0.91, 0.44],
    "k": 5,
    "min_score": 0.7
  }'
```

### Text Search

```bash
curl -X POST http://127.0.0.1:8080/search/text \
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
curl -X POST http://127.0.0.1:8080/search/hybrid \
  -H 'content-type: application/json' \
  -d '{
    "collection": "docs",
    "vector": [0.12, 0.91, 0.44],
    "text_query": "neural networks",
    "k": 10
  }'
```

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
