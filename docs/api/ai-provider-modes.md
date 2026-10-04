# AI provider modes (`red.config.ai.provider`)

RedDB AI consumers select their provider through `REDDB_AI_PROVIDER`,
`red.config.ai.ask.provider`, or `red.config.ai.inference.provider`.
`red.config.ai.provider` (or `REDDB_AI_PROVIDER_MODE`) remains a legacy fallback
when no provider is selected. A mode setting does not replace an explicitly
configured vendor or gateway. New gateway configurations should use the
`openai-compat` or `red-router` provider token and supply both endpoint and key.

## Modes

| Mode token         | Wire protocol                                     | Auth header              | Default base URL            |
|--------------------|---------------------------------------------------|--------------------------|-----------------------------|
| `openai-compat`    | Generic OpenAI-compatible (chat + embeddings)     | `Authorization: Bearer …`| Custom (operator supplied)  |
| `openai-native`    | OpenAI (`api.openai.com`)                         | `Authorization: Bearer …`| `https://api.openai.com/v1` |
| `anthropic-native` | Anthropic Messages API                            | `x-api-key: …`           | `https://api.anthropic.com/v1` |

Hyphen and underscore spellings are both accepted (e.g.
`openai_compat` works too).

## Examples

Set the mode via the HTTP config endpoint:

```bash
curl -X PUT http://127.0.0.1:5000/config/red.config.ai.provider \
  -H 'Content-Type: application/json' \
  -d '{"value":"openai-compat"}'
```

Or via SQL:

```sql
SET CONFIG red.config.ai.provider = 'anthropic-native';
```

When `openai-compat` is the fallback, supply the endpoint at
`red.config.ai.providers.openai-compat.base_url` (or
`REDDB_OPENAI_COMPAT_API_BASE`) and the API key through the Vault or
`REDDB_OPENAI_COMPAT_API_KEY`. Registering through `/ai/credentials` requires
both fields. See [BYOK gateways and RedRouter](../guides/ai-providers.md#byok-gateways-and-redrouter).

## Generic OpenAI-compatible client

The `openai-compat` mode is backed by two engine-internal functions
exposed from `crates/reddb-server/src/ai.rs`:

```rust
pub fn openai_compat_chat(req: OpenAiCompatChatRequest)
    -> RedDBResult<OpenAiCompatChatResponse>;

pub fn openai_compat_embeddings(req: OpenAiCompatEmbeddingsRequest)
    -> RedDBResult<OpenAiCompatEmbeddingsResponse>;
```

Both accept an arbitrary `api_base`, `api_key`, and `extra_headers`,
and return a normalized response with `usage.input_tokens` /
`usage.output_tokens` (chat) or `usage.total_tokens` (embeddings) —
the field names match the Anthropic shape so cost-accounting has one
canonical schema regardless of the upstream provider.

Non-2xx responses are surfaced as `RedDBError::Query` carrying the
status code and the provider's parsed `error.message` (or the raw
body when the provider doesn't return JSON).

## Modality matrix

Separately from the *wire-protocol* mode above, every provider carries a
**modality capability** — which AI jobs it can serve. The four modalities are:

| Modality | Token | What it does |
|:---------|:------|:-------------|
| Embed | `embed` (`embedding`, `embeddings`) | Produce embedding vectors for text |
| Generate | `generate` (`generation`, `chat`, `completion`) | Generate free-form text from a prompt |
| Vision | `vision` (`image`, `multimodal`) | Accept image input alongside text |
| Moderate | `moderate` (`moderation`) | Classify content against a safety taxonomy |

The built-in provider × modality matrix:

| Provider | `embed` | `generate` | `vision` | `moderate` |
|:---------|:-------:|:----------:|:--------:|:----------:|
| `openai` | ✅ | ✅ | ✅ | ✅ |
| `anthropic` | — | ✅ | ✅ | — |
| `minimax` | ✅ | ✅ | ✅ | — |
| `together` | ✅ | ✅ | ✅ | — |
| `ollama` | ✅ | ✅ | ✅ | — |
| `groq` | — | ✅ | ✅ | — |
| `openrouter` | ✅ | ✅ | ✅ | — |
| `venice` | ✅ | ✅ | ✅ | — |
| `deepseek` | — | ✅ | — | — |
| `huggingface` | ✅ | ✅ | — | — |
| `local` | ✅ | — | — | — |
| unknown / `custom` | ✅ | ✅ | — | — |

An **unknown** provider token is treated conservatively: only the universal
text modalities (`embed`, `generate`) are assumed, and `vision`/`moderate`
requests against it are rejected rather than guessed.

The matrix gates a [per-collection AI policy](../query/ai-policy.md) at
**`CREATE TABLE` time**: a policy that wires a provider to a modality it cannot
serve is rejected immediately, not on the first write. Per-deployment overrides
can layer onto the built-in rows when a deployment runs a provider with a
different capability set.

## Relationship to provider task pointers

`red.config.ai.ask.provider` and `red.config.ai.inference.provider` select the
provider identity used for model, endpoint, and credential resolution.
`red.config.ai.embeddings.provider` selects the embedding provider independently.
The legacy mode selector is consulted only when no generation provider has
been selected; `openai-compat` maps to the named `OpenAiCompat` provider rather
than an empty custom provider ID. `red.config.ai.default.provider` is removed;
writing it returns an error naming the task pointer to use instead.
