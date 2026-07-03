# Quickstart: ASK / RAG

Retrieve context from a `collection` (the universal container) and let RedDB
answer a question with citations. **ASK** is the RAG semantic layer: it
retrieves the most relevant items, then grounds an LLM answer in them.

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

## 2. Build a retrievable knowledge base

ASK grounds its answers in whatever it can retrieve. Seed a small vector
collection — real embeddings come from your model; these 2-D vectors keep the
example runnable:

```sql
INSERT INTO kb VECTOR (dense, content) VALUES ([0.9, 0.1], 'RedDB ASK grounds answers in retrieved sources and returns citations.');
INSERT INTO kb VECTOR (dense, content) VALUES ([0.1, 0.9], 'RedWire is the principal transport for RedDB connections.');
INSERT INTO kb VECTOR (dense, content) VALUES ([0.8, 0.2], 'STRICT mode makes ASK refuse to answer when retrieval finds nothing.');
```

## 3. Your first meaningful result: the retrieval

This is exactly the context ASK would ground its answer in — the two nearest
sources to the query embedding:

```sql
VECTOR SEARCH kb SIMILAR TO [0.9, 0.1] LIMIT 2;
```

```text
 content                                                              | score
---------------------------------------------------------------------+------
 RedDB ASK grounds answers in retrieved sources and returns citations.| 0.99
 STRICT mode makes ASK refuse to answer when retrieval finds nothing. | 0.97
```

## 4. Ask, grounded and cited

With an AI provider configured, `ASK` retrieves that same context and returns a
source-cited answer. `[^1]` maps to `sources_flat[0]`; `validation.ok` confirms
every citation points at a real source. This call needs a live provider, so it
is shown here rather than run by the docs test:

```sql
-- doctest:skip
ASK 'how does RedDB ground answers?' USING openai STRICT ON CACHE TTL '5m' LIMIT 5;
```

## Where to go next

- [Vector search quickstart](/getting-started/quickstart-vector.md) — the retrieval half in depth
- [AI policy reference](/query/ai-policy.md) — providers, STRICT mode, and caching
- [Vectors & Embeddings](/data-models/vectors.md) — the collection ASK retrieves from
