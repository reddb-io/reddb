# RAG in 20 lines

Retrieval-augmented generation against RedDB — ingest, embed, retrieve,
and serve source-cited answers through `ASK`, using only SQL. No Python,
no LangChain, no separate vector database.

## The pitch

- **Ingest** text documents into a RedDB collection
- **Embed** their contents via a configured provider (OpenAI, Ollama, …)
- **Ask** a natural-language question and get inline `[^N]` citations
- **Cache** the answer with `CACHE TTL` so repeated questions do not pay
  the provider bill

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

-- 3. retrieve + answer with grounding
ASK 'What does RedDB do?'
  USING openai
  STRICT ON
  CACHE TTL '5m'
  LIMIT 3;
```

That's it. Three statements, end-to-end retrieval over your own data. The ASK
row contains `answer`, `sources_flat`, `citations`, `validation`, provider/model
metadata, token counts, `cost_usd`, and `cache_hit`.

Example shape:

```json
{
  "answer": "RedDB is a multi-model database with native vectors and SQL access[^1].",
  "sources_flat": [
    {
      "kind": "table",
      "urn": "reddb:kb/1",
      "content": "{\"title\":\"RedDB overview\",\"body\":\"...\"}",
      "score": 0.92
    }
  ],
  "citations": [
    { "marker": 1, "span": [69, 73], "urn": "reddb:kb/1" }
  ],
  "validation": { "ok": true, "warnings": [], "errors": [] },
  "provider": "openai",
  "cache_hit": false
}
```

See [ADR 0013](../adr/0013-ask-grounding-citations.md), from tracker
[#392](https://github.com/reddb-io/reddb/issues/392), for the citation and URN
contract behind this shape.

## Caching the answer

`ASK ... CACHE TTL` stores the grounded answer under a deterministic key that
includes the question, provider, model, source fingerprint, temperature, seed,
and tenant. Repeating the same question over stable data can return
`cache_hit: true` without touching the provider:

```sql
ASK 'What does RedDB do?' USING openai STRICT ON CACHE TTL '5m' LIMIT 3;
```

Use `NOCACHE` to bypass a global cache default for a single call.

## Bringing your own model

If you already have trained weights (e.g. a logistic-regression
classifier fine-tuned elsewhere), register them with the model
registry via SQL and then classify inline:

```sql
SELECT MODEL_REGISTER(
  'intent_clf', 'logreg',
  '{"weights":[[...]], "biases":[...], "num_features":384, ...}'
) AS version_id;

SELECT ML_CLASSIFY('intent_clf', EMBED(user_message)) AS intent
FROM   chat_turns;
```

`MODEL_REGISTER` takes JSON produced by the classifier's own
serialisation (`LogisticRegression::to_json()`), so you can train
models in any Rust harness, smoke-test them, and ship the JSON to
production without a side-car service. `MODEL_DROP(name)` archives
every version when you're done.

## What you get for free

| You don't have to | Because |
|-------------------|---------|
| Stand up a vector DB | RedDB speaks vector search natively |
| Wire a separate cache | `ASK ... CACHE TTL` is built in |
| Pre-compute embeddings in Python | `EMBED()` is a SQL scalar |
| Manage model state | `ML_CLASSIFY` / `ML_PREDICT_PROBA` read from
  the built-in model registry |
| Deal with two data stores | The answer, citations, embeddings, and cache
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
