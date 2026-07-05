# AI providers — what works where

RedDB speaks to a roster of AI providers for the `ASK`, `WITH AUTO EMBED`, and
`SEARCH SIMILAR ... USING` flows, plus the per-collection
[AI policy](../query/ai-policy.md). This page documents which provider supports
which capability, the wire shape RedDB uses for each, and how RedDB behaves when
a provider does not offer a capability the operator asked for.

If you came here because `ASK 'something' USING anthropic` for an
embeddings step failed unexpectedly — read the [Anthropic
embeddings](#anthropic-embeddings-policy) section first.

---

## Capability matrix

| Provider    | Token         | API key | Generate / `ASK` | Embed                    | Vision | Moderate | Wire shape            |
|:------------|:--------------|:-------:|:----------------:|:-------------------------|:------:|:--------:|:----------------------|
| OpenAI      | `openai`      | yes     | ✅              | ✅                      | ✅    | ✅      | OpenAI-compat         |
| Anthropic   | `anthropic`   | yes     | ✅              | **rejected** (see below) | ✅    | —       | Anthropic Messages    |
| MiniMax     | `minimax`     | yes     | ✅              | ✅                      | ✅    | —       | OpenAI-compat         |
| Groq        | `groq`        | yes     | ✅              | —                       | ✅    | —       | OpenAI-compat         |
| OpenRouter  | `openrouter`  | yes     | ✅              | —                       | ✅    | —       | OpenAI-compat         |
| Together    | `together`    | yes     | ✅              | ✅                      | ✅    | —       | OpenAI-compat         |
| Venice      | `venice`      | yes     | ✅              | —                       | ✅    | —       | OpenAI-compat         |
| DeepSeek    | `deepseek`    | yes     | ✅              | —                       | —     | —       | OpenAI-compat         |
| HuggingFace | `huggingface` | yes     | ✅              | ✅                      | —     | —       | HF feature-extraction |
| Ollama      | `ollama`      | no      | ✅              | ✅                      | ✅    | —       | OpenAI-compat         |
| Local       | `local`       | no      | —               | feature-gated            | —     | —       | in-process backend    |
| Custom URL  | `https://...` | depends | ✅              | ✅                      | —     | —       | OpenAI-compat         |

The **Generate / Embed / Vision / Moderate** columns are the provider's
[modality capabilities](../api/ai-provider-modes.md#modality-matrix). They gate a
per-collection [AI policy](../query/ai-policy.md) at `CREATE TABLE` time — a
policy that asks a provider for a modality it does not serve is rejected up
front. A `—` in the **Embed** column means RedDB does not route embeddings to
that provider; name an embed-capable provider for the embedding step instead.

> [!NOTE]
> The **Vision** and **Moderate** columns describe declared provider capability.
> The collection-level `VISION` and `MODERATE` policy clauses that consume them
> are still in progress — see [AI policy](../query/ai-policy.md). The `embed`
> and `generate` modalities are live today via `AUTO EMBED`, `ASK`, and the
> `EMBED` policy clause.

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
curl -X POST localhost:5000/ai/embeddings \
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

> The `local` provider runs HuggingFace embedding artifacts in-process,
> *separately* from the hosted HuggingFace Inference API above. The
> register / pull / cache / query lifecycle is documented in
> [Local HuggingFace embeddings](/guides/local-embeddings.md).

---

## Setting a default provider

The AI config namespace (ADR 0068 §5) splits **inference** (generation,
ASK) from **embeddings** (AUTO EMBED, SEARCH SIMILAR TEXT). Two task
pointers say which provider serves each modality:

```sql
-- Task pointers: who generates, who embeds
SET CONFIG red.config.ai.inference.provider  = 'groq'
SET CONFIG red.config.ai.embeddings.provider = 'openai'

-- Per-provider model + base URL live in the provider block
SET CONFIG red.config.ai.providers.groq.models.inference     = 'llama-3.3-70b-versatile'
SET CONFIG red.config.ai.providers.openai.models.embeddings  = 'text-embedding-3-small'
SET CONFIG red.config.ai.providers.groq.base_url             = 'https://api.groq.com/openai/v1'
```

Posting a credential with `"default": true` sets these pointers for you
(the embeddings pointer only when the provider can embed):

```bash
curl -X POST http://127.0.0.1:5000/ai/credentials \
  -d '{"provider":"groq","api_key":"gsk_xxx","default":true}'
```

```sql
-- ASK uses the inference pointer when USING is omitted
ASK 'what changed in the last 24 hours?'
```

**Resolution order for any AI call:** ASK-specific config
(`red.config.ai.ask.*`) → task pointer → the pointed provider's `models`
block → the provider's built-in default.

`ASK … USING <provider>` overrides the **inference** side only —
embeddings still resolve through the embeddings task pointer. A task
pointer aimed at a provider that lacks the modality (Anthropic has no
embeddings API) fails with a didactic error naming the pointer to fix;
there is no silent re-route.

> **Clean break:** `red.config.ai.default.provider`,
> `red.config.ai.default.model`, and the old
> `red.config.ai.{provider}.{alias}.base_url` base-URL shape were removed.
> Writing any of them is rejected with an error naming the new key.

---

## Vault credentials

Every provider in the matrix above (except `local` and `ollama`, which
do not require an API key) resolves its key through the same lookup
chain. As of issue #1270, resolution **prefers the encrypted vault over
environment variables**: the env vars are a zero-config bootstrap fallback so a
fresh deployment can talk to a provider before any key is written to the vault.
Vault-stored keys are encrypted at rest and rotatable through the vault KV path;
env vars carry no such guarantees, which is why they are the fallback rather than
the primary source.

**Canonical vault path:**

```
red.secret.ai.providers.<provider>.tokens.<alias>
```

- `<provider>` matches the **Token** column of the capability matrix
  (`openai`, `huggingface`, `groq`, `together`, `openrouter`,
  `venice`, `deepseek`, `anthropic`).
- `<alias>` is the credential alias. `default` is implicit when a
  request omits `"credential"`.

**Stage a key** (after `CREATE VAULT secrets` + `VAULT UNSEAL`):

```bash
reddb-cli vault set red.secret.ai.providers.openai.tokens.default "sk-..."
reddb-cli vault set red.secret.ai.providers.huggingface.tokens.default "hf_..."
reddb-cli vault set red.secret.ai.providers.openai.tokens.prod "sk-prod-..."
```

Vault setup itself is covered in [Security → Vault](/security/vault.md).

**Resolution order** (first non-empty wins, per request — vault first, env as a
bootstrap fallback):

1. Vault token: `red.secret.ai.providers.<provider>.tokens.<alias>`
2. Vault indirection: `red.config.ai.providers.<provider>.tokens.<alias>.secret_ref` points at another vault path
3. Env var `REDDB_<PROVIDER>_API_KEY_<ALIAS>` (or `REDDB_<PROVIDER>_API_KEY` for the default alias)

For the **default** alias (no `"credential"` in the request), the vault path is
`red.secret.ai.providers.<provider>.tokens.default` and the env fallback is
`REDDB_<PROVIDER>_API_KEY`.

> **Clean break (issue #1745).** The old vault path shape
> (`red.secret.ai.<provider>.<alias>.api_key`) and the deprecated plaintext
> config path (`red.config.ai.<provider>.<alias>.key`) are **removed** — there
> is no deprecation window. A credential still parked at either is rejected
> with a didactic error naming the new vault path to populate; it is never
> silently read. Migrate any staged keys to the `providers.<provider>.tokens.<alias>`
> shape.

A missing key surfaces as a `400` with the exact path the operator
should populate. There is no silent fallback to a different alias or
provider.

**Use the staged key from SQL or HTTP:**

```sql
-- uses red.secret.ai.providers.openai.tokens.default (default alias is implicit)
INSERT INTO docs (body) VALUES ('hello')
  WITH AUTO EMBED (body) USING openai;

ASK 'summarise yesterday' USING openai;
```

```bash
# pick a non-default alias by naming it in the request body
curl -X POST localhost:5000/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{"provider":"openai","inputs":["hi"],"credential":"prod"}'
```

**Plaintext at the boundary is rejected.** `POST /ai/models/{name}/pull`
and the local-embedding pull surface refuse bodies that carry
`api_key`, `token`, `hf_token`, `huggingface_api_key`, etc. The
operator stages the secret in the vault and references it by
`credential_alias` (see [Local HuggingFace
Embeddings](/guides/local-embeddings.md#credentials)).

**Lock secrets down by path with a policy.** Vault paths are
policy-globbable, so an operator can keep an entire AI provider
namespace off-limits — even from admin:

```json
{"effect":"deny","actions":["vault:read","vault:write"],
 "resources":["vault:red.vault/red.secret.ai.providers.custom.*"]}
```

See [Security → Vault](/security/vault.md#locking-down-secrets-by-path-with-policies)
for the full action table and ADR 0027 for the policy-scoping design.

**Indirect reads are audited, not denied.** When a query like
`INSERT … WITH AUTO EMBED USING openai` needs a provider key, the AI
subsystem reads the secret as system (not as the calling user) — the
query layer is the right place to gate "who can run AUTO EMBED?", not
the resolver. Every such read emits an `ai.credential.resolve` audit
event with the principal, provider, alias, and which vault paths were
consulted (never the value).

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
