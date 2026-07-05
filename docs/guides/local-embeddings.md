# Local HuggingFace embeddings

RedDB can compute embeddings **in-process** from a HuggingFace artifact
bundle, with no outbound traffic at query time. This guide covers the
full operating model: feature gating, model registration, artifact
pull/cache lifecycle, query- and write-time usage, credentials, and the
explicit no-silent-fallback contract.

If you only need a hosted-API embedding flow against HuggingFace's
public endpoint, you want
[`huggingface`](/guides/ai-providers.md#huggingface-embeddings), not
`local`. The two are different code paths:

| Capability | Wire                              | Provider token | Network at query time |
|:-----------|:----------------------------------|:---------------|:----------------------|
| Remote HF Inference API | `POST api-inference.huggingface.co/.../{model}` | `huggingface` | yes (every request) |
| Local HF artifact run   | in-process candle (or installed backend)        | `local`       | no (only at pull time) |

`local` reads the bytes off the local filesystem and runs the engine
inside the RedDB server process. Network traffic happens only when an
operator explicitly calls `POST /ai/models/{name}/pull` to acquire
artifacts.

---

## Scope

The `local` provider implements **embeddings only**. Local prompt /
generation / chat / completion inference is **out of scope** for this
slice of the PRD — registering a model with `task: "prompt"`, `"chat"`,
`"generation"`, or `"completion"` is rejected at the registration
boundary, and `POST /ai/prompt` with `provider: "local"` returns
HTTP 400 with an `out of scope` / `embeddings-only` message.

This is a deliberate boundary, not a missing feature. If you need local
generation today, run an Ollama server and point RedDB at it via the
`ollama` provider — same `ASK` and `WITH AUTO EMBED` surfaces, no
RedDB-side weight management.

---

## Feature gating

Local embedding execution is behind the `local-models` Cargo feature on
`reddb-server`. Builds without the flag still understand the `local`
provider token, but every query- or write-time route returns a
deterministic *feature-not-enabled* error and does **not** silently
demote to a different provider.

```bash
# Default build — `local` is recognised but disabled.
cargo build -p reddb-server

# Enable in-process local embeddings.
cargo build -p reddb-server --features local-models
```

When the feature is **off**, the following all surface the same
disabled-feature message (operator-friendly, names the build flag and
the `ollama` workaround):

| Surface                          | Behaviour            |
|:---------------------------------|:---------------------|
| `POST /ai/embeddings` (`provider=local`) | HTTP **501** |
| gRPC `Embeddings` (`provider=local`)     | `feature_not_enabled` |
| `SEARCH SIMILAR ... USING local`         | runtime error |
| `INSERT ... WITH AUTO EMBED ... USING local` | runtime error (no rows written) |
| `POST /ai/prompt` (`provider=local`)     | HTTP **400** (out of scope) |

Feature-disabled behaviour is regression-tested in
`tests/integration_ai_multi_provider.rs` so a `cargo test` run on a
default build keeps these guards honest.

When the feature is **on** but no engine backend has been installed
(via `runtime::ai::local_embedding::install_local_embedding_backend`),
RedDB falls back to a deterministic in-process backend so the wire
contract stays usable in dev builds. Production servers install a real
candle or onnx backend at boot.

---

## Register a model

Local models live in the registry at `red.config.ai.models.{name}`
(KV under the `red_config` collection). Use the HTTP API:

```bash
curl -X POST http://127.0.0.1:5000/ai/models \
  -H 'content-type: application/json' \
  -d '{
    "name": "mini-en",
    "provider": "local",
    "source": "sentence-transformers/all-MiniLM-L6-v2",
    "revision": "v1.0",
    "engine": "candle",
    "task": "embedding",
    "dimensions": 384,
    "pull_policy": "if_missing",
    "trust_policy": "disabled",
    "credential_alias": "hf-public"
  }'
```

Required fields:

| Field | Notes |
|:------|:------|
| `name` | Registry key. `[A-Za-z0-9_-]`, ≤128 chars. |
| `provider` | Must be `local`. Other providers do not register through this endpoint. |
| `source` | HuggingFace repo id or other source identifier. No whitespace. |
| `revision` | Pinned git revision / tag. **No floating refs** (`main`, `latest`). |
| `engine` | Only `candle` is accepted in this slice. |
| `task` | Only `embedding` is accepted. `prompt` / `chat` / `generation` / `completion` are explicitly rejected as out of scope. |
| `dimensions` | Positive integer, 1..=65536. Pins the expected output width. |

Optional fields:

| Field | Default | Notes |
|:------|:--------|:------|
| `pull_policy` | `if_missing` | See [Pull policies](#pull-policies). Legacy spellings (`manual` → `never`, `on_demand` → `if_missing`, `eager` → `always`) are accepted and normalised at write time. |
| `trust_policy` | `disabled` | Set to `allow_remote_code` to opt into model repos that ship custom Python; requires `"acknowledge_remote_code_risk": true` in the same request. |
| `credential_alias` | — | Names a vault-stored HF token. See [Credentials](#credentials). |

**Rejected at the boundary:** any plaintext-credential field
(`api_key`, `token`, `hf_token`, `huggingface_token`, …). The registry
is read by query-time code and must never carry secrets. Use
`credential_alias` plus the vault.

List, inspect, and update:

```bash
curl http://127.0.0.1:5000/ai/models                 # list
curl http://127.0.0.1:5000/ai/models/mini-en         # inspect one
curl -X PUT http://127.0.0.1:5000/ai/models/mini-en \
     -H 'content-type: application/json' \
     -d '{ ...same shape... }'                       # update
```

A freshly registered model has `status: "registered"` — the metadata
exists but no artifacts are on disk yet.

---

## Pull artifacts

Artifact acquisition is an **explicit operator action**:

```bash
curl -X POST http://127.0.0.1:5000/ai/models/mini-en/pull \
  -H 'content-type: application/json' \
  -d '{ "fixture_dir": "/srv/reddb/fixtures/mini-en" }'
```

The pull endpoint:

1. Looks up the model in the registry (404 if missing).
2. Rejects any plaintext-credential field on the request body. Tokens
   must already be in the vault and referenced via `credential_alias`.
3. Resolves provider credentials when `credential_alias` is set (vault
   `red.secret.ai.providers.huggingface.tokens.{alias}` → `secret_ref`
   indirection → env fallback). Empty / missing resolutions are a 400 —
   the operator either staged the secret or did not, no silent fallback.
4. Locates the artifact source. Today RedDB pulls from a local fixture
   directory (`fixture_dir` on the request, or
   `red.config.ai.local.fixture_dir` in `red_config`). Live HuggingFace
   download is a follow-up slice; the surface is wired but does not
   make outbound calls in this slice.
5. Copies every file into a staging dir, hashes each one (SHA-256),
   writes `manifest.json`, then **atomically promotes** the staging dir
   into the model cache. A crash mid-promotion leaves either the old
   or the new artifact bundle on disk — never a half-merged tree.
6. Stamps the registry entry with `status: "installed"`, `cache_dir`,
   `cache_size_bytes`, and `installed_at_unix_ms`.

A successful pull returns the manifest and the resolved `cache_dir`.

### Cache layout

```
{cache_root}/
  {model-name}/
    manifest.json        # name, source, revision, dimensions, total_size_bytes,
                         # installed_at_unix_ms, files[{path, sha256, size_bytes}]
    config.json          # artifact files (whatever the fixture / repo carried)
    tokenizer.json
    model.safetensors
    ...
  .staging/              # transient — promoted into {model-name}/ on success
  .purge/                # transient — replaced installs land here briefly before deletion
```

`cache_root` resolves in this order:

1. `red.config.ai.local.cache_dir` in `red_config` (operator override).
2. `<db-path-parent>/ai_models_cache` (alongside the data files).
3. `$TMPDIR/ai_models_cache` if the engine is running fully in-memory.

### Inspect

```bash
curl http://127.0.0.1:5000/ai/models/mini-en/cache
```

Returns one of:

| `status`      | Meaning |
|:--------------|:--------|
| `installed`   | All manifest files present, sizes match, manifest parses. |
| `missing`     | The cache directory does not exist. The model is `registered` but not pulled. |
| `unhealthy`   | Manifest is unreadable, malformed, or refers to a file whose size diverges. `detail` carries the diagnostic. |

The response always carries `footprint_bytes` (sum of on-disk file
sizes — never lies even when `manifest.json` is corrupt) so dashboards
can plot disk pressure without trusting the manifest.

### Drop cache

```bash
curl -X DELETE http://127.0.0.1:5000/ai/models/mini-en/cache
```

Moves the model directory aside under `.purge/` and removes it. The
registry entry is preserved (`status` reverts to `registered`,
`cache_dir` / `installed_at_unix_ms` / `cache_size_bytes` are cleared)
so a subsequent `POST /ai/models/{name}/pull` re-installs against the
same registration.

---

## Pull policies

`pull_policy` defines what happens when a query-time route resolves a
model whose artifacts are **not installed**. **No policy auto-pulls at
query time in this slice** — they only shape the error message so the
operator knows which knob to turn.

| Policy        | Aliases accepted at write time | Behaviour on missing artifacts |
|:--------------|:-------------------------------|:-------------------------------|
| `never`       | `manual`                       | Hard refuse. Error directs the operator at `POST /ai/models/{name}/pull`. Runtime acquisition is forbidden. |
| `if_missing`  | `on_demand` (legacy)           | **Default.** Same refusal — query-time auto-pull is not implemented. The error explicitly notes the explicit-pull workaround. |
| `always`      | `eager`                        | Same refusal. The error notes that an `always` refresh requires an explicit pull call until the live-pull worker lands. |

All three error variants are `RedDBError::NotFound` and each names the
exact endpoint to call. The point of the distinction is that an
operator can `GET /ai/models/{name}` and see, from the policy alone,
which lifecycle expectation the model was registered under.

---

## Credentials

Local pulls of public HuggingFace repos work with no credential. For
gated / private repos:

1. Stage the token in the vault under the canonical AI secret path:

   ```bash
   reddb-cli vault set red.secret.ai.providers.huggingface.tokens.{alias} "hf_xxx"
   ```

2. Reference it from the model:

   ```json
   { ..., "credential_alias": "{alias}" }
   ```

3. Or per pull request:

   ```bash
   curl -X POST .../ai/models/mini-en/pull \
        -d '{"fixture_dir":"...", "credential_alias":"{alias}"}'
   ```

The resolution order at pull time is vault
(`red.secret.ai.providers.huggingface.tokens.{alias}`) → `secret_ref`
indirection → env fallback (`REDDB_HUGGINGFACE_API_KEY_{ALIAS}`). A non-empty
resolution is required when `credential_alias` is set — RedDB does not silently
fall back to "no auth" if the operator named an alias. The old
`red.secret.ai.huggingface.{alias}.api_key` vault shape and the legacy plaintext
`red.config.ai.huggingface.{alias}.key` path were removed in the same release
(issue #1745, no deprecation window).

**The boundary never accepts plaintext.** Both the model-register
endpoint and the pull endpoint reject `api_key`, `token`, `hf_token`,
`huggingface_token`, and several related field names outright.

The resolved secret is held only in memory long enough to authenticate
the pull. It is never written into the model entry, into the manifest,
or into the HTTP response.

---

## Use it

Once a model is `installed`, the same `local` token routes through
every embedding surface:

### HTTP — direct embedding

```bash
curl -X POST http://127.0.0.1:5000/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "local",
    "model":    "mini-en",
    "inputs":   ["hello world", "second doc"]
  }'
```

Response shape mirrors the OpenAI-compatible payload, with a few extra
fields the local catalog publishes:

```json
{
  "provider":       "local",
  "model":          "mini-en",
  "model_source":   "sentence-transformers/all-MiniLM-L6-v2",
  "model_revision": "v1.0",
  "model_engine":   "candle",
  "dimensions":     384,
  "count":          2,
  "embeddings":     [[...], [...]]
}
```

`model` is **required** for the local provider — there is no implicit
default. Omitting it returns HTTP 400 telling the operator to name a
registered model.

### gRPC

The gRPC `Embeddings` RPC takes the same JSON payload shape and
dispatches through `crate::ai::grpc_embeddings`, which routes
`provider: "local"` into the same in-process backend the HTTP path
uses. Same error variants, same response shape.

### Text vector search

`VECTOR SEARCH ... SIMILAR TO '<text>'` routes through whichever
provider the server is configured to use by default. To route through
the local provider, set the default before issuing the query:

```bash
export REDDB_AI_PROVIDER=local
export REDDB_AI_MODEL=mini-en
```

```sql
VECTOR SEARCH docs SIMILAR TO 'show me the article about AI safety'
  LIMIT 10;
```

The query embedder resolves the model from the registry, runs the
backend once, and hands the resulting dense vector to the vector index.
No remote call.

### Auto-embed inserts

```sql
INSERT INTO docs (id, body)
  VALUES (1, 'AI safety primer ...')
  WITH AUTO EMBED (body)
  USING local MODEL 'mini-en';
```

`MODEL` is required for `USING local`. The auto-embed write path runs
a **pre-flight** against the registry before any row write:

* Feature disabled → reject before opening the target collection.
* Model missing / not registered → reject before opening the target
  collection.
* Wrong task / wrong provider tag / corrupted registry entry → reject.
* Backend output width disagrees with the registered `dimensions` →
  reject before any `create_vector` runs.

The contract: **failures local-side leave the target collection
untouched** — no partial rows, no rollback dance. The integration
suite in `tests/integration_auto_embed_local.rs` pins each of these
behaviours.

> [!TIP]
> A collection can also declare an [`EMBED` policy](../query/ai-policy.md) so
> that **every** write is embedded automatically, asynchronously over CDC,
> without restating `WITH AUTO EMBED`. The end-to-end enrichment path currently
> drives this `local` embedding backend.

---

## No silent fallback

Three rules, taken together, guarantee operators do not get surprised
by a different provider answering on `local`'s behalf:

1. **Feature gating** is deterministic. A build without `local-models`
   refuses every `local` route with the same disabled-feature message
   and never re-routes to a hosted provider.
2. **Registry resolution** is exact. A request for `model: "foo"` that
   is not registered, or is registered but not `installed`, returns
   `NotFound` and names the exact lifecycle action to take.
3. **Pull policies are explicit.** No policy causes RedDB to acquire
   artifacts on its own at query time. `never` / `if_missing` /
   `always` shape the error message — they do not change the answer.

The same principle the cross-provider docs already document for
Anthropic embeddings applies here: a `USING local` query that cannot be
served by the local provider fails fast, with a message that points at
the next operator action.

---

## Tests stay offline

Every test in this crate that touches the `local` provider runs
without network access:

* Registry / cache lifecycle tests
  (`tests/integration_ai_local_models_registry.rs`,
  `tests/integration_ai_local_models_cache.rs`) drive the HTTP
  endpoints against an in-memory runtime with `fixture_dir` pointing
  at a temp directory the test populates itself.
* Text vector search
  (`tests/integration_vector_query_text_local.rs`) installs the
  in-process `DeterministicFakeBackend` (or a controllable fake) and
  exercises the `local` query path end-to-end.
* Auto-embed writes (`tests/integration_auto_embed_local.rs`) use a
  fixed-vector fake to assert the pre-flight, dimension, and
  no-partial-write contracts.
* gRPC / HTTP provider routing
  (`tests/integration_ai_multi_provider.rs`,
  `crates/reddb-server/src/server/handlers_ai.rs`) exercises the
  feature-disabled guards and the in-process backend.

Anything that *does* require network is gated with `#[ignore = "..."]`
(see `tests/integration_ai_live_comment_clustering.rs`). A bare
`cargo test -p reddb-server` will not contact HuggingFace or any
hosted AI provider.
