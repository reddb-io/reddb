# HTTP API

RedDB exposes a comprehensive HTTP/REST API. Start the HTTP server with:

```bash
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:5000
```

## Concurrency Model

The HTTP server spawns one OS thread per accepted TCP connection. Requests are handled concurrently without blocking each other. No configuration is required -- the server detects available CPU cores at startup and automatically skips thread parallelism on single-core machines where the overhead would exceed the gains.

## Result Caching

Identical `SELECT` queries are transparently cached for 30 seconds (max 1000 entries). Cached results return in <1ms. The cache is automatically invalidated whenever an `INSERT`, `UPDATE`, or `DELETE` modifies data, so clients always see fresh results without any extra headers or parameters.

## Health & Status

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/health` | Health check |
| `GET` | `/ready` | Readiness probe |
| `GET` | `/ready/query` | Query readiness |
| `GET` | `/ready/write` | Write readiness |
| `GET` | `/ready/repair` | Repair readiness |
| `GET` | `/ready/serverless` | Serverless readiness (all gates) |
| `GET` | `/ready/serverless/query` | Serverless query readiness |
| `GET` | `/ready/serverless/write` | Serverless write readiness |
| `GET` | `/ready/serverless/repair` | Serverless repair readiness |
| `GET` | `/stats` | Runtime statistics (includes `system` block) |
| `GET` | `/deployment/profiles` | Deployment profile catalog |

### Stats Response

`GET /stats` returns runtime statistics including a `system` block with process and host information:

```json
{
  "active_connections": 2,
  "idle_connections": 14,
  "total_checkouts": 57,
  "paged_mode": true,
  "started_at_unix_ms": 1744329600000,
  "store": {
    "collection_count": 5,
    "total_entities": 12500,
    "total_memory_bytes": 8388608,
    "cross_ref_count": 320
  },
  "system": {
    "pid": 42,
    "cpu_cores": 8,
    "total_memory_bytes": 17179869184,
    "available_memory_bytes": 8589934592,
    "os": "linux",
    "arch": "x86_64",
    "hostname": "db-server-01"
  }
}
```

## Catalog

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/catalog` | Full catalog snapshot with readiness |
| `GET` | `/catalog/readiness` | Catalog readiness (query/write/repair) |
| `GET` | `/catalog/attention` | Items needing attention |
| `GET` | `/catalog/collections/readiness` | **DEPRECATED, sunset 2026-08-08.** Per-collection readiness |
| `GET` | `/catalog/collections/readiness/attention` | **DEPRECATED, sunset 2026-08-08.** Collections needing attention |
| `GET` | `/catalog/consistency` | Consistency report |
| `GET` | `/catalog/indexes/declared` | **DEPRECATED, sunset 2026-08-08.** Declared index definitions |
| `GET` | `/catalog/indexes/operational` | **DEPRECATED, sunset 2026-08-08.** Operational indexes |
| `GET` | `/catalog/indexes/status` | **DEPRECATED, sunset 2026-08-08.** Index artifact statuses |
| `GET` | `/catalog/indexes/attention` | **DEPRECATED, sunset 2026-08-08.** Indexes needing attention |
| `GET` | `/catalog/graph/projections/declared` | **DEPRECATED, sunset 2026-08-08.** Declared graph projections |
| `GET` | `/catalog/graph/projections/operational` | **DEPRECATED, sunset 2026-08-08.** Operational projections |
| `GET` | `/catalog/graph/projections/status` | **DEPRECATED, sunset 2026-08-08.** Projection statuses |
| `GET` | `/catalog/graph/projections/attention` | **DEPRECATED, sunset 2026-08-08.** Projections needing attention |
| `GET` | `/catalog/analytics-jobs/declared` | **DEPRECATED, sunset 2026-08-08.** Declared analytics jobs |
| `GET` | `/catalog/analytics-jobs/operational` | **DEPRECATED, sunset 2026-08-08.** Operational jobs |
| `GET` | `/catalog/analytics-jobs/status` | **DEPRECATED, sunset 2026-08-08.** Job statuses |
| `GET` | `/catalog/analytics-jobs/attention` | **DEPRECATED, sunset 2026-08-08.** Jobs needing attention |

Deprecated granular catalog endpoints return `Deprecation: 2026-08-08` and `Sunset: 2026-08-08`. Use `POST /query` with the corresponding `red.*` SQL relation; see [Deprecated Catalog Endpoints](deprecated-catalog-endpoints.md).

## Physical Layer

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/physical/metadata` | Physical storage metadata |
| `GET` | `/physical/native-header` | Database file header |
| `GET` | `/physical/native-collection-roots` | Collection root pages |
| `GET` | `/physical/native-manifest` | Manifest summary |
| `GET` | `/physical/native-registry` | Registry summary |
| `GET` | `/physical/native-recovery` | Recovery state |
| `GET` | `/physical/native-catalog` | Catalog summary |
| `GET` | `/physical/native-metadata-state` | Metadata state summary |
| `GET` | `/physical/authority` | Physical authority status |
| `GET` | `/physical/native-state` | Full physical state |
| `GET` | `/physical/native-vector-artifacts` | Vector artifact summary |
| `POST` | `/physical/native-header/repair` | Repair database header |
| `POST` | `/physical/native-state/repair` | Repair physical state |
| `POST` | `/physical/metadata/rebuild` | Rebuild physical metadata |

## Entity CRUD

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/collections/{name}/rows` | Create a row |
| `POST` | `/collections/{name}/nodes` | Create a graph node |
| `POST` | `/collections/{name}/edges` | Create a graph edge |
| `POST` | `/collections/{name}/vectors` | Create a vector |
| `POST` | `/collections/{name}/bulk/rows` | Bulk create rows |
| `POST` | `/collections/{name}/bulk/nodes` | Bulk create nodes |
| `POST` | `/collections/{name}/bulk/edges` | Bulk create edges |
| `POST` | `/collections/{name}/bulk/vectors` | Bulk create vectors |
| `POST` | `/collections/{name}/documents` | Create a document |
| `POST` | `/collections/{name}/bulk/documents` | Bulk create documents |
| `GET` | `/collections/{name}/kvs/{key}` | Read a key-value pair by key |
| `PUT` | `/collections/{name}/kvs/{key}` | Create or update a key-value pair |
| `PATCH` | `/collections/{name}/kvs/{key}` | Apply JSON Patch operations to a JSON KV value |
| `DELETE` | `/collections/{name}/kvs/{key}` | Delete a key-value pair by key |
| `PATCH` | `/collections/{name}/entities/{rid}` | Update an item by RedDB ID (supports JSON Patch operations) |
| `DELETE` | `/collections/{name}/entities/{rid}` | Delete an item by RedDB ID |

HTTP item results use the public envelope: `rid` is the RedDB ID, `collection`
is the source collection, and `kind` is one of `row`, `document`, `kv`, `node`,
`edge`, or `vector`. Older public identifier names such as `_entity_id`
and `entity_id` are not part of the public response shape.

### Bulk Insert Performance

The `/collections/{name}/bulk/rows` endpoint uses a fast path with single-lock batching when the paged storage engine is active. For maximum throughput from non-Rust clients, prefer bulk endpoints over individual row inserts.

For zero-overhead ingestion at the highest throughput (241K ops/sec), use the gRPC `BulkInsertBinary` RPC which sends protobuf native types with no JSON serialization. See the [gRPC API docs](grpc.md#binary-bulk-insert) for details.

#### Bulk Insert with AUTO EMBED

Add an `auto_embed` object to the request body to generate embeddings for all rows through the AI batching path before inserting. RedDB collects text across the request, sends provider-sized batches with retry/timeout handling, then links the returned vectors to the inserted rows. If the embedding provider fails after retries, no rows are inserted (502 is returned).

```bash
curl -X POST http://127.0.0.1:5000/collections/articles/bulk/rows \
  -H 'content-type: application/json' \
  -d '{
    "items": [
      {"fields": {"id": 1, "title": "hello world"}},
      {"fields": {"id": 2, "title": "another document"}}
    ],
    "auto_embed": {
      "provider": "openai",
      "fields": ["title"],
      "model": "text-embedding-3-small"
    }
  }'
```

Response includes embedding stats:

```json
{"ok": true, "created_count": 2, "embedded_count": 2, "provider_requests": 1}
```

- `created_count` — rows inserted
- `embedded_count` — vectors created (rows with at least one non-empty embed field)
- `provider_requests` — provider calls made for embeddings; normally 1, higher only when the batch exceeds the provider chunk size

Omitting `auto_embed` preserves the legacy behavior (`{"ok": true, "count": N}`).

### Documents

Create a document entity with an arbitrary JSON body:

```bash
curl -X POST http://127.0.0.1:5000/collections/logs/documents \
  -H 'content-type: application/json' \
  -d '{"body": {"level": "info", "message": "test"}}'
```

Bulk create multiple documents in one request:

```bash
curl -X POST http://127.0.0.1:5000/collections/logs/bulk/documents \
  -H 'content-type: application/json' \
  -d '{"items": [
    {"body": {"level": "info", "message": "first"}},
    {"body": {"level": "warn", "message": "second"}}
  ]}'
```

### Key-Value Pairs

Read a key-value pair by key:

```bash
curl -s http://127.0.0.1:5000/collections/settings/kvs/theme
```

Create or update a key-value pair:

```bash
curl -X PUT http://127.0.0.1:5000/collections/settings/kvs/theme \
  -H 'content-type: application/json' \
  -d '{"value": "dark"}'
```

Delete a key-value pair by key:

```bash
curl -X DELETE http://127.0.0.1:5000/collections/settings/kvs/theme
```

### JSON Patch & path helpers

`PATCH /collections/{name}/entities/{rid}` and
`PATCH /collections/{name}/kvs/{key}` accept a JSON Patch-shaped body so a UI
can apply nested edits without re-sending the whole document or value:

```json
{
  "dry_run": false,
  "operations": [
    { "op": "set",   "path": "/body/prefs/lang", "value": "en" },
    { "op": "unset", "path": "/body/meta/ip" }
  ]
}
```

| Field | Type | Notes |
|:--|:--|:--|
| `operations` | `array` | List of operations applied in order. Empty / omitted = no-op. |
| `operations[].op` | `string` | `set` (alias `add`), `replace`, or `unset` (aliases `remove`, `delete`). |
| `operations[].path` | `string` | JSON Pointer (RFC 6901). `~1` escapes `/`, `~0` escapes `~`. |
| `operations[].value` | any | Required for `set` / `replace`. Must be omitted for `unset`. |
| `dry_run` | `bool` | When `true`, validate operations and return `{ok, dry_run:true, operations:N}` without mutating. |

Document patch (`/entities/{rid}`) targets the row fields and the document
body (`/body/...`); intermediate objects are created on missing nested paths,
and `unset` of a missing path is a no-op success.

KV patch (`/kvs/{key}`) requires the stored value to be a JSON object or array
(or a `Value::Json` blob produced by `PUT /kvs/{key}` with an object/array
`value`). Scalar KV values (integer, boolean, text, null) reject nested
patches with a `KV_VALUE_NOT_JSON` error so Red UI can route to the
replace-whole-value workflow.

#### Structured error envelope

Validation and apply failures surface a UI-safe envelope rather than a plain
string error:

```json
{
  "ok": false,
  "code": "PATCH_PATH_INVALID",
  "error": "patch path contains empty segment",
  "message": "patch path contains empty segment",
  "op_index": 0,
  "pointer": "/body//ip"
}
```

| Code | When raised |
|:--|:--|
| `PATCH_BODY_INVALID` | `operations` / `dry_run` is the wrong JSON type. |
| `PATCH_OP_INVALID` | Unknown `op`, missing `value` on `set`, `value` present on `unset`. |
| `PATCH_PATH_INVALID` | Empty path, empty segment, or otherwise un-pointer-able. |
| `PATCH_APPLY_FAILED` | Engine refused the patch (type conflict, validation, etc.). |
| `KV_KEY_NOT_FOUND` | KV patch target key does not exist. `pointer` omitted. |
| `KV_VALUE_NOT_JSON` | Stored KV value is not a JSON object / array. |
| `NOT_FOUND` | Document patch target row does not exist. |

`op_index` and `pointer` are present whenever the failure can be tied back to
a specific operation, so an editor can highlight the offending field.

### TTL over HTTP

For entity writes, use top-level control fields:

- `ttl`: relative duration such as `60`, `"60s"`, `"5m"`, `"250ms"`
- `ttl_ms`: relative duration in milliseconds
- `expires_at`: absolute expiration in Unix epoch milliseconds

Examples:

```bash
curl -X POST http://127.0.0.1:5000/collections/sessions/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"token":"t-1","user_id":"u-1"},"ttl":"15m"}'

curl -X PATCH http://127.0.0.1:5000/collections/sessions/entities/1 \
  -H 'content-type: application/json' \
  -d '{"ttl":"30m"}'

curl -X PATCH http://127.0.0.1:5000/collections/sessions/entities/1 \
  -H 'content-type: application/json' \
  -d '{"operations":[{"op":"set","path":"ttl","value":"0s"}]}'
```

## Query & Search

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/query` | Execute SQL/universal query |
| `POST` | `/context` | Unified context search across all data structures |
| `POST` | `/collections/{name}/similar` | Vector similarity search in a collection |
| `POST` | `/collections/{name}/ivf/search` | IVF approximate search in a collection |
| `POST` | `/text/search` | Full-text search |
| `POST` | `/multimodal/search` | Global multimodal lookup (table, document, kv, vector, graph) |
| `POST` | `/hybrid/search` | Hybrid text + vector search |
| `POST` | `/search` | Unified search (`mode=auto|index|multimodal|hybrid`) |

### SQL Query with Safe Parameters

`POST /query` accepts positional `$N` bind values in a top-level `params`
array. Use this shape for user input and vectors instead of building SQL
strings in the client. The cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

```bash
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{
    "query": "SELECT * FROM hosts WHERE critical = $1 LIMIT $2",
    "params": [true, 10]
  }'
```

Vector parameters use a JSON number array:

```bash
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{
    "query": "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2",
    "params": [[0.12, 0.91, 0.44], 5]
  }'
```

### Exact numeric body representation

RedDB has three numeric body classes on JSON transports:

- JSON integers in the signed safe range `[-9007199254740991, 9007199254740991]`
  are exact integers and may appear as normal JSON numbers.
- Finite JSON numbers with a fractional part or exponent that fit native `f64`
  are floating-point numbers and may appear as normal JSON numbers.
- Exact integers outside the signed safe range, unsigned integers outside the
  signed safe range, and decimal text values use typed envelopes.

Drivers must use the typed envelopes below in `params`, row/document bodies,
query results, and any JSON-RPC payloads whenever a normal JSON number would
not round-trip safely through every official driver:

```json
{"$int":"9007199254740993"}
{"$uint":"9223372036854775808"}
{"$decimal":"3.141592653589793238462643383279"}
```

The envelope values are base-10 strings with no grouping separators. `$int`
must fit `i64`; `$uint` must fit `u64`; `$decimal` must be a valid JSON number
token and is stored as canonical decimal text. The superseded exact-number
envelopes `$number` and `$decimalText` are invalid for this release: official
drivers must reject them instead of converting them to float or string.

### Streaming reads with resumable cursors (`POST /query/stream`)

`POST /query/stream` streams a read-only `SELECT` as newline-delimited JSON
(`application/x-ndjson`) over chunked transfer encoding. Frames arrive in a
fixed order: a `descriptor` frame first, then a `cursor` control frame, then
one `row` frame per record, then a terminal `end` frame.

> The full normative contract — descriptor-first emission, chunk format,
> cursor lifecycle, cancellation, disconnect semantics, snapshot
> consistency, ordering guarantees, tenant/authorization scope, TTL,
> expiry, and the read-only limit — lives in one place:
> [Query Streaming Contract](query-streaming.md). The examples below
> illustrate it.

The `cursor` frame carries an **opaque resume token** scoped to the calling
tenant and principal and pinned to the read snapshot:

```json
{"cursor":{"token":"<48-hex>","snapshot_lsn":42,"ttl_ms":60000,"expires_at_ms":1750000060000,"resumable":true}}
```

Treat `token` as opaque — it encodes nothing the client should parse.

**Resuming.** To re-stream the pinned view, POST the token back. No `query`
field is needed — the pinned query and snapshot live server-side:

```bash
curl -X POST http://127.0.0.1:5000/query/stream \
  -H 'content-type: application/json' \
  -H 'x-reddb-tenant: acme' \
  -H 'authorization: Bearer <principal-token>' \
  -d '{"cursor": "<token from the cursor frame>"}'
```

A resume replays the same descriptor-first stream.

**Scope and expiry semantics.**

- The cursor is bound to the `(tenant, principal)` that opened it. The tenant
  comes from the `x-reddb-tenant` header (or the bearer credential); the
  principal comes from the bearer credential.
- A token that is unknown — **or** presented by a different tenant or
  principal — is refused with `404 cursor_not_found`. The response is
  identical in both cases, so an unauthorized caller cannot tell a foreign
  cursor from one that never existed (no existence leak).
- A token whose TTL has elapsed is refused to its rightful owner with
  `410 cursor_expired`. Open a new stream to obtain a fresh cursor.
- The TTL defaults to `stream.snapshot.ttl_ms` (60s); tune it via
  `PUT /config/stream.snapshot.ttl_ms`.

Refusals are non-streaming JSON responses (no chunked body), so a client can
distinguish "the stream was never accepted" from a mid-stream failure.

**Cancellation.** A long-running read can be cancelled two ways; both signal
the executor to stop producing rows and tombstone the cursor.

- *Explicit cancel.* POST the cursor token to `POST /query/stream/cancel`:

  ```bash
  curl -X POST http://127.0.0.1:5000/query/stream/cancel \
    -H 'content-type: application/json' \
    -H 'x-reddb-tenant: acme' \
    -H 'authorization: Bearer <principal-token>' \
    -d '{"cursor": "<token from the cursor frame>"}'
  ```

  A matched cursor returns `200 {"ok":true,"status":"cancelled"}`. Cancel is
  scoped exactly like resume: an unknown or foreign token is masked as
  `404 cursor_not_found`, so cancellation cannot be used to probe for cursors.
  Cancel is idempotent — cancelling an already-cancelled cursor still
  returns `200`.

- *Client disconnect.* If the client closes the TCP connection mid-stream,
  the server detects the broken pipe, raises the same executor cancel signal,
  and tombstones the cursor — it does not keep computing rows for a dead
  client.

When a cancel is observed while the stream is still draining, the stream
terminates with a documented terminal frame in place of `end`:

```json
{"cancelled":{"row_count":<rows emitted so far>,"reason":"cancelled"}}
```

**Resuming a cancelled cursor is refused** with `409 cursor_cancelled` to its
owner — distinct from `410 cursor_expired` (aged out) and `404
cursor_not_found` (unknown/foreign) — so the client learns the stream was
cancelled rather than retrying a snapshot it abandoned. Open a new stream to
start over.

### Context Search

`POST /context` performs a unified context search across all data structures (tables, graphs, vectors, documents, key-value pairs). It follows cross-references and optionally expands graph neighborhoods in a single request.

Only the `query` field is required. All other fields are optional and control how deep and wide the search reaches.

```bash
curl -X POST http://127.0.0.1:5000/context \
  -H 'content-type: application/json' \
  -d '{
    "query": "AB1234567",
    "field": "passport",
    "collections": ["customers"],
    "graph_depth": 1,
    "graph_max_edges": 20,
    "max_cross_refs": 10,
    "follow_cross_refs": true,
    "expand_graph": true,
    "global_scan": true,
    "reindex": true,
    "limit": 25,
    "min_score": 0.0
  }'
```

| Parameter | Type | Default | Description |
|:----------|:-----|:--------|:------------|
| `query` | `string` | *(required)* | Search term or value to look up |
| `field` | `string` | `null` | Restrict table/document matching to a specific field |
| `collections` | `string[]` | `null` | Limit search to these collections |
| `graph_depth` | `integer` | `1` | How many hops to traverse when expanding graph results |
| `graph_max_edges` | `integer` | `20` | Maximum edges returned per graph expansion |
| `max_cross_refs` | `integer` | `10` | Maximum cross-references to follow |
| `follow_cross_refs` | `boolean` | `true` | Whether to follow cross-references between entities |
| `expand_graph` | `boolean` | `true` | Whether to expand graph neighborhoods around matched nodes |
| `global_scan` | `boolean` | `true` | Scan all collections when `collections` is not specified |
| `reindex` | `boolean` | `true` | Re-index before searching to include recent writes |
| `limit` | `integer` | `25` | Maximum results per structure type |
| `min_score` | `float` | `0.0` | Minimum relevance score for returned results |

The response groups results by structure type:

```json
{
  "ok": true,
  "tables": [...],
  "graph": { "nodes": [...], "edges": [...] },
  "vectors": [...],
  "documents": [...],
  "key_values": [...],
  "connections": [...],
  "summary": { ... }
}
```

## Configuration

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/config` | Export all configuration as nested JSON |
| `POST` | `/config` | Import configuration from a JSON tree |
| `GET` | `/config/{key}` | Read a single config key or subtree |
| `PUT` | `/config/{key}` | Set a config key (scalar or JSON subtree) |
| `DELETE` | `/config/{key}` | Delete a config key |

Configuration is stored as dot-notation key-value pairs in the `red_config` collection. The `/config` endpoints automatically flatten JSON trees on import and nest them on export.

### Export Configuration

`GET /config` returns the full configuration tree as nested JSON:

```bash
curl http://127.0.0.1:5000/config
```

Response:

```json
{
  "ok": true,
  "config": {
    "red": {
      "ai": {
        "default": {
          "provider": "groq",
          "model": "llama-3.3-70b-versatile"
        },
        "groq": {
          "default": { "key": "gsk_xxx" }
        }
      }
    }
  }
}
```

### Import Configuration

`POST /config` accepts a JSON tree and flattens it into dot-notation KV pairs:

```bash
curl -X POST http://127.0.0.1:5000/config \
  -H 'content-type: application/json' \
  -d '{
    "red": {
      "ai": {
        "default": { "provider": "ollama", "model": "llama3" }
      }
    }
  }'
```

Response:

```json
{"ok": true, "imported": 2, "keys": ["red.ai.default.provider", "red.ai.default.model"]}
```

### Individual Config Keys

Read a single key or a subtree by path:

```bash
curl http://127.0.0.1:5000/config/red.ai.default.provider
```

If the key is an exact match, the response returns that value. If the key is a prefix, the response returns the nested subtree.

Set a single scalar value:

```bash
curl -X PUT http://127.0.0.1:5000/config/red.ai.default.provider \
  -H 'content-type: application/json' \
  -d '{"value": "groq"}'
```

Set a subtree (the server flattens it into individual keys):

```bash
curl -X PUT http://127.0.0.1:5000/config/red.storage.hnsw \
  -H 'content-type: application/json' \
  -d '{"value": {"m": 32, "ef_search": 100}}'
```

Delete a config key:

```bash
curl -X DELETE http://127.0.0.1:5000/config/red.ai.default.model
```

### Configuration via SQL

You can also manage configuration through the query engine:

```bash
# SET CONFIG via SQL
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "SET CONFIG red.ai.default.provider = '\''groq'\''"}'

# SHOW CONFIG via SQL
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "SHOW CONFIG"}'

# SHOW CONFIG subtree via SQL
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "SHOW CONFIG red.ai"}'
```

## AI

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/ai/embeddings` | Generate embeddings via the pooled AI transport (single, batch, query-row, query-result) |
| `POST` | `/ai/prompt` | Execute prompts via the pooled AI transport (single, batch, query-row, query-result) |
| `POST` | `/ai/ask` | One-shot question against the database using any provider |
| `POST` | `/ai/credentials` | Store provider API keys by alias in KV (`red_config`) |

### Supported Providers

RedDB ships with a multi-provider AI layer. Every provider that exposes an OpenAI-compatible API works out of the box. Set the `provider` field on any `/ai/*` request to route to a specific backend.

| Provider | Token | API Base | Env Key | Embedding | Prompt |
|:---------|:------|:---------|:--------|:----------|:-------|
| OpenAI | `openai` | `api.openai.com/v1` | `REDDB_OPENAI_API_KEY` | yes | yes |
| Anthropic | `anthropic` | `api.anthropic.com/v1` | `REDDB_ANTHROPIC_API_KEY` | no | yes |
| Groq | `groq` | `api.groq.com/openai/v1` | `REDDB_GROQ_API_KEY` | yes | yes |
| OpenRouter | `openrouter` | `openrouter.ai/api/v1` | `REDDB_OPENROUTER_API_KEY` | yes | yes |
| Together | `together` | `api.together.xyz/v1` | `REDDB_TOGETHER_API_KEY` | yes | yes |
| Venice | `venice` | `api.venice.ai/api/v1` | `REDDB_VENICE_API_KEY` | yes | yes |
| DeepSeek | `deepseek` | `api.deepseek.com/v1` | `REDDB_DEEPSEEK_API_KEY` | yes | yes |
| Ollama | `ollama` | `localhost:11434/v1` | *(none)* | yes | yes |
| HuggingFace | `huggingface` | `api-inference.huggingface.co` | `REDDB_HF_API_KEY` | yes | yes |
| Local | `local` | *(embedded)* | *(none)* | stub | stub |
| Custom URL | *(url)* | user-defined | `REDDB_CUSTOM_API_KEY` | yes | yes |

### Credential Configuration

`POST /ai/credentials` stores API keys in the RedDB vault (the `red_config` KV collection). You can store keys by provider name and optional alias.

Credentials are persisted as dot-notation keys in the `red_config` collection:

| Key Pattern | Example | Description |
|:------------|:--------|:------------|
| `red.ai.{provider}.{alias}.key` | `red.ai.groq.default.key` | API key for a provider/alias |
| `red.ai.{provider}.{alias}.base_url` | `red.ai.custom.prod.base_url` | Base URL override |
| `red.ai.default.provider` | `red.ai.default.provider` | Default provider for all AI requests |
| `red.ai.default.model` | `red.ai.default.model` | Default model for all AI requests |

Store an API key:

```bash
curl -X POST http://127.0.0.1:5000/ai/credentials \
  -H 'content-type: application/json' \
  -d '{"provider": "groq", "api_key": "gsk_xxx"}'
```

Store with a custom base URL:

```bash
curl -X POST http://127.0.0.1:5000/ai/credentials \
  -H 'content-type: application/json' \
  -d '{"provider": "custom", "api_key": "sk-xxx", "api_base": "https://my-proxy.com/v1"}'
```

Store with an alias:

```bash
curl -X POST http://127.0.0.1:5000/ai/credentials \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "api_key": "sk-...",
    "alias": "prod",
    "metadata": {"owner":"platform","rotation":"2026-04"}
  }'
```

Set a provider as the default (so you can omit `USING` in queries):

```bash
curl -X POST http://127.0.0.1:5000/ai/credentials \
  -H 'content-type: application/json' \
  -d '{"provider": "groq", "api_key": "gsk_xxx", "default": true}'
```

You can also set the default provider and model through other methods:

- **Config endpoint**: `POST /config` with `{"red":{"ai":{"default":{"provider":"groq","model":"llama-3.3-70b-versatile"}}}}`
- **Environment variables**: `REDDB_AI_PROVIDER` and `REDDB_AI_MODEL`

### Credential Resolution Chain

When a request includes a `credential` alias, RedDB resolves the API key using the following chain. The first match wins.

1. **Environment variable with alias**: `REDDB_{PROVIDER}_API_KEY_{ALIAS}` (e.g. `REDDB_OPENAI_API_KEY_PROD`)
2. **RedDB vault**: KV key `{provider}/{alias}` in the `red_config` collection
3. **Default environment variable**: `REDDB_{PROVIDER}_API_KEY` (e.g. `REDDB_GROQ_API_KEY`)
4. **Default vault entry**: KV key `{provider}/default` in the `red_config` collection

### Embeddings

`POST /ai/embeddings` request modes:

- direct input: `input` or `inputs`
- query row mode: `source_query` + `source_mode: "row"` + `source_field`
- query result mode: `source_query` + `source_mode: "result"`

All modes send one provider batch per request unless provider chunk limits require splitting. The deprecated synchronous Rust helpers are compatibility shims; server paths use `AiBatchClient` for AUTO EMBED and the pooled `AiTransport` path for direct embedding and prompt endpoints.

Optional persistence:

- `save.collection`: vector collection to persist generated embeddings
- `save.include_content`: include original input text in vector `content` (default `true`)
- `save.metadata`: metadata object applied to each saved vector

Direct input example:

```bash
curl -X POST http://127.0.0.1:5000/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "text-embedding-3-small",
    "input": "critical linux host running ssh",
    "credential": "prod",
    "save": {
      "collection": "embeddings",
      "include_content": true,
      "metadata": {"source": "manual"}
    }
  }'
```

Per-row example from query output:

```bash
curl -X POST http://127.0.0.1:5000/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "together",
    "model": "togethercomputer/m2-bert-80M-8k-retrieval",
    "source_query": "SELECT description FROM incidents LIMIT 100",
    "source_mode": "row",
    "source_field": "description",
    "credential": "prod",
    "max_inputs": 100,
    "save": {"collection": "incident_embeddings"}
  }'
```

Per-result example (single embedding for the whole result):

```bash
curl -X POST http://127.0.0.1:5000/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "text-embedding-3-small",
    "source_query": "SELECT * FROM incidents WHERE severity = '\''high'\'' LIMIT 200",
    "source_mode": "result",
    "credential": "prod"
  }'
```

### Prompts

`POST /ai/prompt` request modes:

- direct prompt: `prompt` or `prompts`
- query row mode: `source_query` + `source_mode: "row"` + (`prompt_template` or `source_field`)
- query result mode: `source_query` + `source_mode: "result"` (+ optional `prompt_template`)

Optional persistence for prompt outputs:

- `save.collection`: collection for generated outputs
- `save.prompt_field`: row field name for original prompt (default `prompt`)
- `save.response_field`: row field name for model response (default `response`)
- `save.metadata`: metadata object applied to each saved row

Groq prompt from query rows:

```bash
curl -X POST http://127.0.0.1:5000/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "groq",
    "model": "llama-3.3-70b-versatile",
    "source_query": "SELECT ip, risk FROM hosts LIMIT 20",
    "source_mode": "row",
    "prompt_template": "Classify the risk of host {{ip}} with score {{risk}} in one sentence.",
    "credential": "prod",
    "save": {"collection": "host_risk_summaries"}
  }'
```

Anthropic prompt over full query result:

```bash
curl -X POST http://127.0.0.1:5000/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "anthropic",
    "model": "claude-3-5-haiku-latest",
    "source_query": "SELECT * FROM incidents WHERE severity = '\''high'\'' LIMIT 200",
    "source_mode": "result",
    "prompt_template": "Summarize the main findings:\n{{result}}",
    "credential": "ops"
  }'
```

OpenAI prompt (direct):

```bash
curl -X POST http://127.0.0.1:5000/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "gpt-4.1-mini",
    "prompt": "Explain the difference between HNSW and IVF indexes."
  }'
```

Ollama prompt (local, no API key required):

```bash
curl -X POST http://127.0.0.1:5000/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "ollama",
    "model": "llama3",
    "prompt": "List three advantages of multi-model databases."
  }'
```

### ASK

`POST /ai/ask` is a convenience endpoint that sends a natural-language question
to a provider and returns the canonical non-streaming ASK envelope. New clients
should consume `sources_flat`, `citations`, and `validation`; `[^N]` markers in
`answer` map to `sources_flat[N-1].urn`. The grounding contract is defined in
[ADR 0013](../../.red/adr/0013-ask-grounding-citations.md), from
[#392](https://github.com/reddb-io/reddb/issues/392).

```bash
curl -X POST http://127.0.0.1:5000/ai/ask \
  -H 'content-type: application/json' \
  -d '{
    "question": "What happened with the last deployment?",
    "provider": "groq",
    "model": "llama-3.3-70b-versatile"
  }'
```

Using Ollama (no credentials needed):

```bash
curl -X POST http://127.0.0.1:5000/ai/ask \
  -H 'content-type: application/json' \
  -d '{
    "question": "Summarize the incidents from last week",
    "provider": "ollama",
    "model": "llama3"
  }'
```

Using OpenRouter:

```bash
curl -X POST http://127.0.0.1:5000/ai/ask \
  -H 'content-type: application/json' \
  -d '{
    "question": "Which hosts have the highest risk score?",
    "provider": "openrouter",
    "model": "meta-llama/llama-3.3-70b-instruct",
    "credential": "prod"
  }'
```

Response shape:

```json
{
  "answer": "The last deployment failed after the API pod hit a missing secret[^1].",
  "sources_flat": [
    {
      "kind": "table",
      "urn": "reddb:deploy_events/42",
      "content": "{\"service\":\"api\",\"error\":\"missing secret\"}",
      "score": 0.94
    }
  ],
  "citations": [
    { "marker": 1, "span": [69, 73], "urn": "reddb:deploy_events/42" }
  ],
  "validation": { "ok": true, "warnings": [], "errors": [] },
  "cache_hit": false,
  "provider": "groq",
  "model": "llama-3.3-70b-versatile",
  "prompt_tokens": 512,
  "completion_tokens": 24,
  "cost_usd": 0.0012,
  "mode": "strict",
  "retry_count": 0
}
```

For SQL-style controls such as `STRICT`, `USING`, `CACHE TTL`, `NOCACHE`,
`TEMPERATURE`, `SEED`, `LIMIT`, `MIN_SCORE`, and `DEPTH`, use `/query`:

```bash
curl -N -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "ASK '\''why did deploy fail?'\'' USING '\''groq,openai'\'' STRICT ON STREAM CACHE TTL '\''5m'\'' LIMIT 5"}'
```

When `STREAM` is present, `/query` returns `text/event-stream` frames in this
order: `sources`, zero or more `answer_token`, then `validation`.

### Environment Configuration

Each provider follows the same naming convention. Replace `{PROVIDER}` with the uppercase provider token (e.g. `OPENAI`, `GROQ`, `TOGETHER`).

- `REDDB_AI_PROVIDER`: default provider when a request omits `provider`
- `REDDB_{PROVIDER}_API_KEY`: default API key for the provider
- `REDDB_{PROVIDER}_API_KEY_{ALIAS}`: API key for a specific credential alias
- `REDDB_{PROVIDER}_API_BASE`: base URL override (useful for proxies or self-hosted endpoints)
- `REDDB_{PROVIDER}_EMBEDDING_MODEL`: default embedding model for `/ai/embeddings`
- `REDDB_{PROVIDER}_PROMPT_MODEL`: default prompt model for `/ai/prompt`
- `REDDB_ANTHROPIC_VERSION`: Anthropic API version header (default `2023-06-01`)

## Backup & Recovery

### CDC -- Change Data Capture

Poll real-time change events from the database. Every entity CRUD operation (insert, update, delete) emits an event into the CDC buffer.

```
GET /changes?since_lsn=0&limit=100
```

| Parameter | Type | Default | Description |
|:----------|:-----|:--------|:------------|
| `since_lsn` | `integer` | `0` | Cursor position -- return events after this LSN |
| `limit` | `integer` | `100` | Maximum events to return (max `10000`) |

Response:

```json
{
  "ok": true,
  "events": [
    {"lsn": 1, "timestamp": 1744329600000, "operation": "insert", "collection": "users", "rid": 42, "kind": "row"},
    {"lsn": 2, "operation": "update", "collection": "config", "rid": 7, "kind": "kv"}
  ],
  "next_lsn": 2
}
```

Operations: `insert`, `update`, `delete`. CDC events identify changed items with
RedDB ID `rid`, source `collection`, and item `kind`. Use `next_lsn` as the
`since_lsn` value in your next request to consume events incrementally.

### Backups

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/backup/status` | Scheduler status, last backup timestamp, backup history |
| `POST` | `/backup/trigger` | Force an immediate backup outside the scheduled interval |

Check backup status:

```bash
curl http://127.0.0.1:5000/backup/status
```

Trigger a manual backup:

```bash
curl -X POST http://127.0.0.1:5000/backup/trigger
```

### Recovery

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/recovery/restore-points` | List available restore points |

List restore points:

```bash
curl http://127.0.0.1:5000/recovery/restore-points
```

### Backup & Recovery Configuration

You can enable and tune backup, WAL archiving, and CDC through the runtime configuration endpoint:

```bash
# Enable scheduled backups
curl -X PUT localhost:5000/config/red.backup.enabled -d '{"value": true}'
curl -X PUT localhost:5000/config/red.backup.interval_secs -d '{"value": 3600}'

# Enable WAL archiving
curl -X PUT localhost:5000/config/red.wal.archive.enabled -d '{"value": true}'

# CDC is enabled by default
curl localhost:5000/config/red.cdc
```

See [Configuration -- Backup & Recovery](/getting-started/configuration.md#backup--recovery-redbackup) for the full list of keys.

## Graph Analytics

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/graph/traverse` | BFS/DFS traversal |
| `POST` | `/graph/shortest-path` | Shortest path |
| `POST` | `/graph/neighborhood` | Node neighborhood |
| `POST` | `/graph/analytics/components` | Connected components |
| `POST` | `/graph/analytics/centrality` | Centrality scores |
| `POST` | `/graph/analytics/community` | Community detection |
| `POST` | `/graph/analytics/clustering` | Clustering coefficient |
| `POST` | `/graph/analytics/pagerank/personalized` | Personalized PageRank |
| `POST` | `/graph/analytics/hits` | HITS algorithm |
| `POST` | `/graph/analytics/cycles` | Cycle detection |
| `POST` | `/graph/analytics/topological-sort` | Topological ordering |
| `GET` | `/graph/projections` | List projections |
| `GET` | `/graph/jobs` | List analytics jobs |

## Snapshots & Operations

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/manifest` | Get manifest |
| `GET` | `/roots` | Collection roots |
| `GET` | `/snapshots` | List snapshots |
| `GET` | `/exports` | List exports |
| `POST` | `/snapshot` | Create a snapshot |
| `POST` | `/export` | Create a named export |
| `POST` | `/tick` | Run maintenance/reclaim tick (maintenance, retention, checkpoint) |
| `POST` | `/retention/apply` | Apply retention policy |
| `POST` | `/checkpoint` | Force WAL checkpoint |

## Auth

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/auth/bootstrap` | Bootstrap first admin user |
| `POST` | `/auth/login` | Login and get session token |
| `POST` | `/auth/users` | Create a user |
| `GET` | `/auth/users` | List all users |
| `POST` | `/auth/api-keys` | Create an API key |
| `POST` | `/auth/change-password` | Change password |
| `GET` | `/auth/whoami` | Get current user info |

## Replication

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/replication/status` | Replication status |
| `POST` | `/replication/snapshot` | Replication snapshot |

## Git for Data (VCS)

Full reference: [/vcs/commands.md](/vcs/commands.md). Surface is
RESTful and collection-centric — resources live under `/repo/*`
(refs, commits, sessions, merges) and `/collections/{name}/vcs`
(opt-in toggle).

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET`    | `/repo` | Repo summary (branches, tags, versioned collections) |
| `GET`    | `/repo/refs[?prefix=refs/heads/]` | Unified ref listing |
| `GET`    | `/repo/refs/heads` | List branches |
| `POST`   | `/repo/refs/heads` | Create branch `{name, from?, connection_id?}` |
| `GET`    | `/repo/refs/heads/{name}` | Show branch |
| `PUT`    | `/repo/refs/heads/{name}` | Move branch `{commit}` |
| `DELETE` | `/repo/refs/heads/{name}` | Delete branch |
| `GET`    | `/repo/refs/tags` / `POST` / `GET {name}` / `DELETE {name}` | Tag CRUD |
| `GET`    | `/repo/commits?branch=&limit=&skip=&from=&to=&no_merges=` | Commit log |
| `POST`   | `/repo/commits` | Create commit from session workset |
| `GET`    | `/repo/commits/{hash}` | Show commit |
| `GET`    | `/repo/commits/{a}/diff/{b}[?collection=&summary=true]` | Diff |
| `GET`    | `/repo/commits/{a}/lca/{b}` | Lowest common ancestor |
| `GET`    | `/repo/sessions/{conn}` | Workset status |
| `POST`   | `/repo/sessions/{conn}/checkout` | Switch HEAD |
| `POST`   | `/repo/sessions/{conn}/merge` | Merge into HEAD |
| `POST`   | `/repo/sessions/{conn}/reset` | Reset HEAD |
| `POST`   | `/repo/sessions/{conn}/cherry-pick` | Apply one commit |
| `POST`   | `/repo/sessions/{conn}/revert` | Reverse one commit |
| `GET`    | `/repo/merges/{msid}` | Merge-state summary |
| `GET`    | `/repo/merges/{msid}/conflicts` | List unresolved conflicts |
| `POST`   | `/repo/merges/{msid}/conflicts/{cid}/resolve` | Resolve one |
| `GET`    | `/collections/{name}/vcs` | Is this collection opted in? |
| `PUT`    | `/collections/{name}/vcs` | Opt in / out `{versioned}` |

Status codes: 201 Created (commit/branch/tag), 204 No Content
(delete, resolve), 400 invalid body, 404 unknown ref / commit,
409 protected branch / conflict, 405 method not allowed.

Time-travel queries use the SQL `AS OF` clause (not a separate
endpoint):

```sql
SELECT * FROM users AS OF COMMIT '7a1a...' WHERE age > 21;
```

## DDL

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/collections` | Create a collection |
| `DELETE` | `/collections/{name}` | Drop a collection |
| `GET` | `/collections/{name}/schema` | Describe collection schema |

Collection creation accepts `ttl` or `ttl_ms` as the default retention policy:

```bash
curl -X POST http://127.0.0.1:5000/collections \
  -H 'content-type: application/json' \
  -d '{"name":"sessions","ttl":"60m"}'
```

`GET /collections/{name}/schema` now returns `default_ttl_ms` and `default_ttl` when configured.

## Example: Full Workflow

```bash
# 1. Check health
curl -s http://127.0.0.1:5000/health

# 2. Insert a row
curl -X POST http://127.0.0.1:5000/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Alice", "age": 30}}'

# 3. Query
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users"}'

# 4. Create snapshot
curl -X POST http://127.0.0.1:5000/snapshot

# 5. Check stats
curl -s http://127.0.0.1:5000/stats
```

> [!NOTE]
> All HTTP endpoints return JSON. Error responses follow the format `{"ok": false, "error": "description"}` with appropriate HTTP status codes.
