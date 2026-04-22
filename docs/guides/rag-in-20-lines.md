# RAG in 20 lines

Retrieval-augmented generation against RedDB — ingest, embed, retrieve,
and serve answers through the semantic cache, using only SQL. No
Python, no LangChain, no separate vector database.

## The pitch

- **Ingest** text documents into a RedDB collection
- **Embed** their contents via a configured provider (OpenAI, Ollama, …)
- **Search** the closest documents for a user question
- **Cache** the answer so the next identical question doesn't pay the
  provider bill

Every step is a single SQL statement.

## Prerequisites

Configure the AI provider once:

```sql
SET CONFIG ai.default_provider = 'openai';
SET CONFIG ai.openai.api_key = 'sk-...';
```

For Ollama running locally, swap the provider and skip the api_key:

```sql
SET CONFIG ai.default_provider = 'ollama';
SET CONFIG ai.ollama.api_base = 'http://localhost:11434/v1';
```

## The 20 lines

```sql
-- 1. schema
CREATE TABLE kb (
  id INT PRIMARY KEY,
  title TEXT,
  body  TEXT,
  embedding VECTOR(1536)
);

-- 2. ingest + embed
INSERT INTO kb (id, title, body, embedding)
SELECT id, title, body, EMBED(body)
FROM   ingest_staging;

-- 3. retrieve + answer
WITH q AS (
  SELECT EMBED('What does RedDB do?') AS emb
)
SELECT ML_CLASSIFY('answer_relevance', kb.embedding) AS relevant,
       kb.title,
       kb.body
FROM   kb, q
ORDER BY VECTOR_DISTANCE(kb.embedding, q.emb)
LIMIT 3;
```

That's it. Three statements, end-to-end retrieval over your own data.

## Caching the answer

`SEMANTIC_CACHE_GET` returns a previously cached response when the
incoming question is close enough (cosine similarity above the
configured threshold). `SEMANTIC_CACHE_PUT` writes one. Together they
give you a semantic-deduplicated answer cache without touching Redis
or a Python side-car:

```sql
-- miss → NULL, hit → the cached text
SELECT SEMANTIC_CACHE_GET('qa-default', EMBED('What does RedDB do?')) AS cached;

-- populate after a successful LLM call
SELECT SEMANTIC_CACHE_PUT(
  'qa-default',
  'What does RedDB do?',
  'RedDB is an AI-first multi-model database …',
  EMBED('What does RedDB do?')
);
```

Next time the same (or near-identical) question arrives, the cache
short-circuits the embed → retrieve → generate loop.

## What you get for free

| You don't have to | Because |
|-------------------|---------|
| Stand up a vector DB | RedDB speaks vector search natively |
| Wire a separate cache | `SEMANTIC_CACHE_*` is built in |
| Pre-compute embeddings in Python | `EMBED()` is a SQL scalar |
| Manage model state | `ML_CLASSIFY` / `ML_PREDICT_PROBA` read from
  the built-in model registry |
| Deal with two data stores | The answer, the embeddings, and the cache
  live in the same transaction domain |

## Gotchas

- **EMBED is synchronous.** Calling it on every row of a 10M-row
  ingest will hit your provider 10M times. Use it when loading (batched
  in the SELECT above) or inside materialised views refreshed on a
  schedule — not on the hot path of analytical queries.
- **Provider required.** Without `ai.default_provider` configured (or
  an explicit second arg), `EMBED` returns `NULL`.
- **Model registry is in-memory today.** Versions you `register_version`
  via the Rust API survive the process but not yet a restart; durable
  model persistence is a follow-up.

## Next steps

- [ASK your database](ask-your-database.md) — full semantic search
  tutorial (context indexes, auto-embed columns, natural-language
  questions)
- [Vector engine](/vectors/engine.md) — indexes, quantisation, ANN
  parameters
- [Semantic cache internals](/data-models/semantic-cache.md) — LRU,
  TTL, cosine threshold tuning *(coming soon)*
