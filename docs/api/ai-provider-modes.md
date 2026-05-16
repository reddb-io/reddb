# AI provider modes (`red.config.ai.provider`)

RedDB engine-side AI consumers (currently `AskPipeline`) can talk to
three different wire-protocol families. The mode is selected by the
`red.config.ai.provider` config key (or the `REDDB_AI_PROVIDER_MODE`
environment variable, which wins). It is intentionally separate from
`red.config.ai.default.provider`, which names a *vendor*
(`openai`, `groq`, `ollama`, ...); the mode key answers the prior
question of *which HTTP shape to speak*.

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
curl -X PUT http://127.0.0.1:8080/config/red.config.ai.provider \
  -H 'Content-Type: application/json' \
  -d '{"value":"openai-compat"}'
```

Or via SQL:

```sql
SET CONFIG red.config.ai.provider = 'anthropic-native';
```

When `openai-compat` is selected the operator is expected to also
supply the target endpoint via `red.config.ai.{vendor}.{alias}.base_url`
(or the matching `REDDB_*_API_BASE` env var). Existing vendor-native
paths are left untouched — switching the mode does not silently
re-route requests to a different vendor than the one configured.

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

## Relationship to `red.config.ai.default.provider`

* `red.config.ai.default.provider` → names a vendor (`openai`,
  `groq`, `ollama`, ...) and is used to pick default models,
  default base URLs, and credential aliases.
* `red.config.ai.provider` → picks the wire-protocol family.
  When set, it takes precedence in `resolve_default_provider` and
  maps the three mode tokens onto the matching `AiProvider`
  variant (`OpenAi`, `Anthropic`, or a `Custom` placeholder for
  `openai-compat`).
