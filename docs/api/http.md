# HTTP API

RedDB exposes a comprehensive HTTP/REST API. Start the HTTP server with:

```bash
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:8080
```

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
| `GET` | `/stats` | Runtime statistics |
| `GET` | `/deployment/profiles` | Deployment profile catalog |

## Catalog

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/catalog` | Full catalog snapshot with readiness |
| `GET` | `/catalog/readiness` | Catalog readiness (query/write/repair) |
| `GET` | `/catalog/attention` | Items needing attention |
| `GET` | `/catalog/collections/readiness` | Per-collection readiness |
| `GET` | `/catalog/collections/readiness/attention` | Collections needing attention |
| `GET` | `/catalog/consistency` | Consistency report |
| `GET` | `/catalog/indexes/declared` | Declared index definitions |
| `GET` | `/catalog/indexes/operational` | Operational indexes |
| `GET` | `/catalog/indexes/status` | Index artifact statuses |
| `GET` | `/catalog/indexes/attention` | Indexes needing attention |
| `GET` | `/catalog/graph/projections/declared` | Declared graph projections |
| `GET` | `/catalog/graph/projections/operational` | Operational projections |
| `GET` | `/catalog/graph/projections/status` | Projection statuses |
| `GET` | `/catalog/graph/projections/attention` | Projections needing attention |
| `GET` | `/catalog/analytics-jobs/declared` | Declared analytics jobs |
| `GET` | `/catalog/analytics-jobs/operational` | Operational jobs |
| `GET` | `/catalog/analytics-jobs/status` | Job statuses |
| `GET` | `/catalog/analytics-jobs/attention` | Jobs needing attention |

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
| `DELETE` | `/collections/{name}/kvs/{key}` | Delete a key-value pair by key |
| `PATCH` | `/collections/{name}/entities/{id}` | Update an entity |
| `DELETE` | `/collections/{name}/entities/{id}` | Delete an entity |

### Documents

Create a document entity with an arbitrary JSON body:

```bash
curl -X POST http://127.0.0.1:8080/collections/logs/documents \
  -H 'content-type: application/json' \
  -d '{"body": {"level": "info", "message": "test"}}'
```

Bulk create multiple documents in one request:

```bash
curl -X POST http://127.0.0.1:8080/collections/logs/bulk/documents \
  -H 'content-type: application/json' \
  -d '{"items": [
    {"body": {"level": "info", "message": "first"}},
    {"body": {"level": "warn", "message": "second"}}
  ]}'
```

### Key-Value Pairs

Read a key-value pair by key:

```bash
curl -s http://127.0.0.1:8080/collections/settings/kvs/theme
```

Create or update a key-value pair:

```bash
curl -X PUT http://127.0.0.1:8080/collections/settings/kvs/theme \
  -H 'content-type: application/json' \
  -d '{"value": "dark"}'
```

Delete a key-value pair by key:

```bash
curl -X DELETE http://127.0.0.1:8080/collections/settings/kvs/theme
```

### TTL over HTTP

For entity writes, use top-level control fields:

- `ttl`: relative duration such as `60`, `"60s"`, `"5m"`, `"250ms"`
- `ttl_ms`: relative duration in milliseconds
- `expires_at`: absolute expiration in Unix epoch milliseconds

Examples:

```bash
curl -X POST http://127.0.0.1:8080/collections/sessions/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"token":"t-1","user_id":"u-1"},"ttl":"15m"}'

curl -X PATCH http://127.0.0.1:8080/collections/sessions/entities/1 \
  -H 'content-type: application/json' \
  -d '{"ttl":"30m"}'

curl -X PATCH http://127.0.0.1:8080/collections/sessions/entities/1 \
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

### Context Search

`POST /context` performs a unified context search across all data structures (tables, graphs, vectors, documents, key-value pairs). It follows cross-references and optionally expands graph neighborhoods in a single request.

Only the `query` field is required. All other fields are optional and control how deep and wide the search reaches.

```bash
curl -X POST http://127.0.0.1:8080/context \
  -H 'content-type: application/json' \
  -d '{
    "query": "081.232.036-08",
    "field": "cpf",
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

## AI

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/ai/embeddings` | Generate embeddings via OpenAI (single, batch, query-row, query-result) |
| `POST` | `/ai/prompt` | Execute prompts via OpenAI or Anthropic (single, batch, query-row, query-result) |
| `POST` | `/ai/credentials` | Store provider API keys by alias in KV (`__ai_credentials`) |

`POST /ai/embeddings` request modes:

- direct input: `input` or `inputs`
- query row mode: `source_query` + `source_mode: "row"` + `source_field`
- query result mode: `source_query` + `source_mode: "result"`

Optional persistence:

- `save.collection`: vector collection to persist generated embeddings
- `save.include_content`: include original input text in vector `content` (default `true`)
- `save.metadata`: metadata object applied to each saved vector

Direct input example:

```bash
curl -X POST http://127.0.0.1:8080/ai/embeddings \
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
curl -X POST http://127.0.0.1:8080/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "text-embedding-3-small",
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
curl -X POST http://127.0.0.1:8080/ai/embeddings \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "text-embedding-3-small",
    "source_query": "SELECT * FROM incidents WHERE severity = '\''high'\'' LIMIT 200",
    "source_mode": "result",
    "credential": "prod"
  }'
```

Credential resolution order for OpenAI:

1. `credential` alias env var: `REDDB_OPENAI_API_KEY_<ALIAS>`
2. `credential` alias KV key: collection `__ai_credentials`, key `openai/<alias>`
3. default env var: `REDDB_OPENAI_API_KEY`
4. default KV key: collection `__ai_credentials`, key `openai/default`

`POST /ai/prompt` request modes:

- direct prompt: `prompt` or `prompts`
- query row mode: `source_query` + `source_mode: "row"` + (`prompt_template` or `source_field`)
- query result mode: `source_query` + `source_mode: "result"` (+ optional `prompt_template`)

Optional persistence for prompt outputs:

- `save.collection`: collection for generated outputs
- `save.prompt_field`: row field name for original prompt (default `prompt`)
- `save.response_field`: row field name for model response (default `response`)
- `save.metadata`: metadata object applied to each saved row

OpenAI prompt from query rows:

```bash
curl -X POST http://127.0.0.1:8080/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "model": "gpt-4.1-mini",
    "source_query": "SELECT ip, risk FROM hosts LIMIT 20",
    "source_mode": "row",
    "prompt_template": "Classifique o risco do host {{ip}} com score {{risk}} em 1 frase.",
    "credential": "prod",
    "save": {"collection": "host_risk_summaries"}
  }'
```

Anthropic prompt over full query result:

```bash
curl -X POST http://127.0.0.1:8080/ai/prompt \
  -H 'content-type: application/json' \
  -d '{
    "provider": "anthropic",
    "model": "claude-3-5-haiku-latest",
    "source_query": "SELECT * FROM incidents WHERE severity = '\''high'\'' LIMIT 200",
    "source_mode": "result",
    "prompt_template": "Resuma os principais achados:\n{{result}}",
    "credential": "ops"
  }'
```

Credential resolution order for Anthropic:

1. `credential` alias env var: `REDDB_ANTHROPIC_API_KEY_<ALIAS>`
2. `credential` alias KV key: collection `__ai_credentials`, key `anthropic/<alias>`
3. default env var: `REDDB_ANTHROPIC_API_KEY`
4. default KV key: collection `__ai_credentials`, key `anthropic/default`

Store provider credentials in KV:

```bash
curl -X POST http://127.0.0.1:8080/ai/credentials \
  -H 'content-type: application/json' \
  -d '{
    "provider": "openai",
    "alias": "prod",
    "api_key": "sk-...",
    "metadata": {"owner":"platform","rotation":"2026-04"}
  }'
```

Environment configuration (optional):

- `REDDB_AI_PROVIDER`: default provider when request omits `provider` (`openai` or `anthropic`)
- `REDDB_OPENAI_API_KEY`: default OpenAI API key
- `REDDB_OPENAI_API_KEY_<ALIAS>`: OpenAI key for alias credential
- `REDDB_OPENAI_API_BASE`: OpenAI base URL (default `https://api.openai.com/v1`)
- `REDDB_OPENAI_EMBEDDING_MODEL`: default embedding model for `/ai/embeddings`
- `REDDB_OPENAI_PROMPT_MODEL`: default prompt model for `/ai/prompt` when `provider=openai`
- `REDDB_ANTHROPIC_API_KEY`: default Anthropic API key
- `REDDB_ANTHROPIC_API_KEY_<ALIAS>`: Anthropic key for alias credential
- `REDDB_ANTHROPIC_API_BASE`: Anthropic base URL (default `https://api.anthropic.com/v1`)
- `REDDB_ANTHROPIC_VERSION`: Anthropic API version header (default `2023-06-01`)
- `REDDB_ANTHROPIC_PROMPT_MODEL`: default prompt model for `/ai/prompt` when `provider=anthropic`

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

## DDL

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/collections` | Create a collection |
| `DELETE` | `/collections/{name}` | Drop a collection |
| `GET` | `/collections/{name}/schema` | Describe collection schema |

Collection creation accepts `ttl` or `ttl_ms` as the default retention policy:

```bash
curl -X POST http://127.0.0.1:8080/collections \
  -H 'content-type: application/json' \
  -d '{"name":"sessions","ttl":"60m"}'
```

`GET /collections/{name}/schema` now returns `default_ttl_ms` and `default_ttl` when configured.

## Example: Full Workflow

```bash
# 1. Check health
curl -s http://127.0.0.1:8080/health

# 2. Insert a row
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Alice", "age": 30}}'

# 3. Query
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users"}'

# 4. Create snapshot
curl -X POST http://127.0.0.1:8080/snapshot

# 5. Check stats
curl -s http://127.0.0.1:8080/stats
```

> [!NOTE]
> All HTTP endpoints return JSON. Error responses follow the format `{"ok": false, "error": "description"}` with appropriate HTTP status codes.
