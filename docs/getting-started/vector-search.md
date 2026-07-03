# Vector Search Quickstart

Use this when similarity is the primary access pattern. The Collection is the
universal container; the vector model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE VECTOR product_embeddings DIM 2 METRIC cosine;
INSERT INTO product_embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'red running shoe');
INSERT INTO product_embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'blue hiking boot');
VECTOR SEARCH product_embeddings SIMILAR TO [1.0, 0.0] LIMIT 1;
```

First meaningful result: the final query returns the nearest product embedding.

Where to go next: [Vectors & Embeddings](/data-models/vectors.md),
[Search Commands](/query/search-commands.md), and [HNSW Index](/vectors/hnsw.md).
