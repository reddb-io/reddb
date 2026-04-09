# Building a Vector Search App

This guide shows how to build a semantic search application using RedDB's vector engine.

## Architecture

```mermaid
flowchart LR
    U[User Query] --> E[Embedding Model]
    E --> V[Query Vector]
    V --> R[RedDB Similar Search]
    R --> S[Ranked Results]
    S --> U
```

## 1. Start RedDB

```bash
red server --http --path ./data/search.rdb --bind 127.0.0.1:8080
```

## 2. Index Documents

For each document, generate an embedding (using OpenAI, Cohere, or any embedding model) and insert it:

```bash
# Document 1
curl -X POST http://127.0.0.1:8080/collections/articles/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.45, 0.78, 0.23, 0.56, 0.89, 0.34, 0.67],
    "content": "Introduction to machine learning and neural networks",
    "metadata": {"title": "ML Basics", "author": "Alice", "category": "tutorial"}
  }'

# Document 2
curl -X POST http://127.0.0.1:8080/collections/articles/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.91, 0.23, 0.56, 0.78, 0.12, 0.45, 0.89, 0.34],
    "content": "Database indexing strategies for optimal query performance",
    "metadata": {"title": "DB Indexing", "author": "Bob", "category": "database"}
  }'
```

## 3. Bulk Index

For production, use bulk insert:

```bash
curl -X POST http://127.0.0.1:8080/collections/articles/bulk/vectors \
  -H 'content-type: application/json' \
  -d '[
    {"dense": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8], "content": "Doc 1", "metadata": {"cat": "a"}},
    {"dense": [0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1], "content": "Doc 2", "metadata": {"cat": "b"}}
  ]'
```

## 4. Search

Generate an embedding for the user's query and search:

```bash
curl -X POST http://127.0.0.1:8080/search/similar \
  -H 'content-type: application/json' \
  -d '{
    "collection": "articles",
    "vector": [0.15, 0.42, 0.75, 0.20, 0.58, 0.85, 0.30, 0.65],
    "k": 5,
    "min_score": 0.5
  }'
```

## 5. Hybrid Search

Combine vector similarity with text matching:

```bash
curl -X POST http://127.0.0.1:8080/search/hybrid \
  -H 'content-type: application/json' \
  -d '{
    "collection": "articles",
    "vector": [0.15, 0.42, 0.75, 0.20, 0.58, 0.85, 0.30, 0.65],
    "text_query": "machine learning",
    "k": 10
  }'
```

## 6. Text-Only Search

When you don't have an embedding model available:

```bash
curl -X POST http://127.0.0.1:8080/search/text \
  -H 'content-type: application/json' \
  -d '{
    "query": "database indexing performance",
    "collections": ["articles"],
    "limit": 10,
    "fuzzy": true
  }'
```

## Tips

- **Dimension consistency**: All vectors in a collection should have the same dimension
- **Normalization**: Cosine similarity works best with normalized vectors
- **Metadata filtering**: Use metadata to filter results by category, date, or author
- **Hybrid search**: Combines the semantic understanding of vectors with the precision of keyword matching
