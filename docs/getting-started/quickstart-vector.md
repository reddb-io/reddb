# Quickstart: Vector Search

Store embeddings and find the nearest ones by similarity. The **vector**
model is a semantic layer over a `collection` (the universal container): each
item holds a `dense` vector plus its `content`, and RedDB ranks them by
distance to a query vector.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Insert embeddings

Real embeddings have hundreds of dimensions; these 2-D vectors keep the math
easy to read:

```sql
INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway runbook');
INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual');
INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.5, 0.5], 'shared note');
```

## 3. Your first meaningful result

Ask for the vectors most similar to `[1.0, 0.0]`:

```sql
VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 2;
```

```text
 content         | score
-----------------+------
 gateway runbook | 1.00
 shared note     | 0.71
```

## Where to go next

- [Vectors & Embeddings](/data-models/vectors.md) — the full vector model
- [HNSW index](/vectors/hnsw.md) — approximate search at scale
- [ASK / RAG quickstart](/getting-started/quickstart-ask-rag.md) — grounded answers over vectors
