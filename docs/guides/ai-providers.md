# AI providers — what works where

RedDB speaks to 11 AI providers for the `ASK`, `WITH AUTO EMBED`, and
`SEARCH SIMILAR ... USING` flows. This page documents which provider
supports which capability, the wire shape RedDB uses for each, and
how RedDB behaves when a provider does not offer a capability the
operator asked for.

If you came here because `ASK 'something' USING anthropic` for an
embeddings step failed unexpectedly — read the [Anthropic
embeddings](#anthropic-embeddings-policy) section first.

---

## Capability matrix

| Provider    | Token         | API key | Prompt / `ASK` | Embeddings           | Wire shape            |
|:------------|:--------------|:-------:|:--------------:|:---------------------|:----------------------|
| OpenAI      | `openai`      | yes     | ✅            | ✅                  | OpenAI-compat         |
| Anthropic   | `anthropic`   | yes     | ✅            | **rejected** (see below) | Anthropic Messages    |
| Groq        | `groq`        | yes     | ✅            | ✅                  | OpenAI-compat         |
| OpenRouter  | `openrouter`  | yes     | ✅            | ✅                  | OpenAI-compat         |
| Together    | `together`    | yes     | ✅            | ✅                  | OpenAI-compat         |
| Venice      | `venice`      | yes     | ✅            | ✅                  | OpenAI-compat         |
| DeepSeek    | `deepseek`    | yes     | ✅            | ✅                  | OpenAI-compat         |
| HuggingFace | `huggingface` | yes     | ✅            | ✅                  | HF feature-extraction |
| Ollama      | `ollama`      | no      | ✅            | ✅                  | OpenAI-compat         |
| Local       | `local`       | no      | feature-gated  | feature-gated        | candle in-process     |
| Custom URL  | `https://...` | depends | ✅            | ✅                  | OpenAI-compat         |

`OpenAI-compat` means the provider exposes `POST {base}/embeddings`
and `POST {base}/chat/completions` with the OpenAI shape, so RedDB
ships the same payload and just changes the base URL + auth header.

---

## HuggingFace embeddings

HF's embeddings endpoint is **not** OpenAI-compatible. RedDB has a
dedicated HF client that targets:

```
POST https://api-inference.huggingface.co/pipeline/feature-extraction/{model}
Authorization: Bearer $HF_API_KEY
Content-Type: application/json

{ "inputs": "the text to embed" }
```

The response is a flat JSON array of floats per input. RedDB issues
one request per input, accumulates results, and returns them in the
same shape an OpenAI-compat provider would.

Default model: `sentence-transformers/all-MiniLM-L6-v2`. Override
with `REDDB_HUGGINGFACE_EMBEDDING_MODEL` or per-request `model:`
field.

```bash
curl -X POST localhost:8080/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "huggingface",
    "model": "sentence-transformers/all-MiniLM-L6-v2",
    "inputs": ["hello world"],
    "credential": "default"
  }'
```

Same shape works through `WITH AUTO EMBED ... USING huggingface` in
SQL.

---

## Anthropic embeddings policy

**Anthropic does not offer an embeddings API.** Their own
documentation directs users to OpenAI, Voyage, or another embedding
service for that step.

RedDB does not silently re-route an embeddings request from Anthropic
to a different provider. Doing so would mask configuration bugs and
produce surprising bills against an API the operator did not name.
Instead, the request fails fast with:

> Anthropic does not offer an embeddings API. Re-issue the request
> against an OpenAI-compatible provider (openai, groq, ollama,
> openrouter, together, venice, deepseek), HuggingFace, or a custom
> base URL — RedDB does not silently route embeddings to a different
> provider than the one you named.

Operator workaround: name an OpenAI-compatible provider for the
embeddings step, even if the prompt step uses Anthropic.

```sql
-- prompt via Anthropic, embeddings via OpenAI
INSERT INTO articles (body) VALUES ('AI safety...')
  WITH AUTO EMBED (body) USING openai;

ASK 'summarize the latest articles' USING anthropic;
```

Same applies to `Local` when the `local-models` feature flag is not
compiled in: fail fast rather than silently demote.

---

## Setting a default provider

```bash
# Set default provider — drops `USING` from every query
curl -X POST http://127.0.0.1:8080/ai/credentials \
  -d '{"provider":"groq","api_key":"gsk_xxx","default":true}'
```

```sql
-- ASK uses the default provider when USING is omitted
ASK 'what changed in the last 24 hours?'
```

Default provider can be overridden per-call with `USING <provider>`.

---

## Environment variables

| Variable                            | Default                       | Purpose                  |
|:------------------------------------|:------------------------------|:-------------------------|
| `REDDB_OPENAI_API_KEY`              | —                             | OpenAI API key           |
| `REDDB_OPENAI_API_BASE`             | `https://api.openai.com/v1`   | Override OpenAI URL      |
| `REDDB_OPENAI_EMBEDDING_MODEL`      | `text-embedding-3-small`      | Override OpenAI default  |
| `REDDB_HUGGINGFACE_API_KEY`         | —                             | HF API key               |
| `REDDB_HUGGINGFACE_EMBEDDING_MODEL` | `sentence-transformers/...`   | Override HF default      |
| `REDDB_ANTHROPIC_API_KEY`           | —                             | Anthropic API key (prompt only) |
| `REDDB_GROQ_API_KEY`                | —                             | Groq API key             |
| ... (per-provider, same pattern)    |                               |                          |

Per-credential aliases use `REDDB_{PROVIDER}_API_KEY_{ALIAS}` —
the alias is mentioned in the request as `"credential": "..."`.
