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
| `POST` | `/physical/repair-header` | Repair database header |
| `POST` | `/physical/repair-state` | Repair physical state |
| `POST` | `/physical/rebuild-metadata` | Rebuild physical metadata |

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
| `PATCH` | `/collections/{name}/entities/{id}` | Update an entity |
| `DELETE` | `/collections/{name}/entities/{id}` | Delete an entity |

## Query & Search

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/query` | Execute SQL/universal query |
| `POST` | `/search/similar` | Vector similarity search |
| `POST` | `/search/ivf` | IVF approximate search |
| `POST` | `/search/text` | Full-text search |
| `POST` | `/search/hybrid` | Hybrid text + vector search |

## Graph Analytics

| Method | Path | Description |
|:-------|:-----|:------------|
| `POST` | `/graph/traverse` | BFS/DFS traversal |
| `POST` | `/graph/shortest-path` | Shortest path |
| `POST` | `/graph/neighborhood` | Node neighborhood |
| `POST` | `/graph/components` | Connected components |
| `POST` | `/graph/centrality` | Centrality scores |
| `POST` | `/graph/community` | Community detection |
| `POST` | `/graph/clustering` | Clustering coefficient |
| `POST` | `/graph/personalized-pagerank` | Personalized PageRank |
| `POST` | `/graph/hits` | HITS algorithm |
| `POST` | `/graph/cycles` | Cycle detection |
| `POST` | `/graph/topological-sort` | Topological ordering |
| `GET` | `/graph/projections` | List projections |
| `GET` | `/graph/jobs` | List analytics jobs |

## Snapshots & Operations

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/manifest` | Get manifest |
| `GET` | `/roots` | Collection roots |
| `GET` | `/snapshots` | List snapshots |
| `GET` | `/exports` | List exports |
| `POST` | `/snapshots` | Create a snapshot |
| `POST` | `/exports` | Create a named export |
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
| `GET` | `/collections/{name}/describe` | Describe collection schema |

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
curl -X POST http://127.0.0.1:8080/snapshots

# 5. Check stats
curl -s http://127.0.0.1:8080/stats
```

> [!NOTE]
> All HTTP endpoints return JSON. Error responses follow the format `{"ok": false, "error": "description"}` with appropriate HTTP status codes.
