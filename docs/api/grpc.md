# gRPC API

RedDB exposes a comprehensive gRPC API via the `reddb.v1.RedDb` service. All RPCs use Protocol Buffers and are accessible through any gRPC client.

## Starting the gRPC Server

```bash
red server --grpc --path ./data/reddb.rdb --bind 0.0.0.0:50051
```

## Testing with grpcurl

```bash
# Health check
grpcurl -plaintext 127.0.0.1:50051 reddb.v1.RedDb/Health

# List collections
grpcurl -plaintext 127.0.0.1:50051 reddb.v1.RedDb/Collections
```

## RPC Reference

### Health & Status

| RPC | Request | Description |
|:----|:--------|:------------|
| `Health` | `Empty` | Health check with state |
| `Ready` | `Empty` | Readiness probe |
| `Stats` | `Empty` | Runtime statistics (collections, entities, memory, connections) |
| `CatalogReadiness` | `Empty` | Query/write/repair readiness |
| `DeploymentProfiles` | `DeploymentProfileRequest` | Get deployment profile details |
| `CollectionReadiness` | `Empty` | Per-collection readiness |
| `CollectionAttention` | `Empty` | Collections needing attention |
| `CatalogAttentionSummary` | `Empty` | Catalog-wide attention summary |
| `CatalogConsistency` | `Empty` | Consistency report |

### Entity CRUD

| RPC | Request | Description |
|:----|:--------|:------------|
| `CreateRow` | `JsonCreateRequest` | Insert a table row |
| `CreateNode` | `JsonCreateRequest` | Insert a graph node |
| `CreateEdge` | `JsonCreateRequest` | Insert a graph edge |
| `CreateVector` | `JsonCreateRequest` | Insert a vector embedding |
| `BulkCreateRows` | `JsonBulkCreateRequest` | Bulk insert rows |
| `BulkCreateNodes` | `JsonBulkCreateRequest` | Bulk insert nodes |
| `BulkCreateEdges` | `JsonBulkCreateRequest` | Bulk insert edges |
| `BulkCreateVectors` | `JsonBulkCreateRequest` | Bulk insert vectors |
| `CreateDocument` | `JsonCreateRequest` | Create a document entity |
| `CreateKv` | `JsonCreateRequest` | Create a key-value pair |
| `BulkCreateDocuments` | `JsonBulkCreateRequest` | Bulk create documents |
| `PatchEntity` | `UpdateEntityRequest` | Update an entity by ID |
| `DeleteEntity` | `DeleteEntityRequest` | Delete an entity by ID |

### TTL over gRPC

Entity RPCs keep using `JsonCreateRequest` and `UpdateEntityRequest`, but the JSON payload now accepts:

- `ttl`: relative duration such as `60`, `"60s"`, `"5m"`, `"250ms"`
- `ttl_ms`: relative duration in milliseconds
- `expires_at`: absolute expiration in Unix epoch milliseconds

Examples:

```bash
grpcurl -plaintext \
  -d '{"collection":"sessions","payloadJson":"{\"fields\":{\"token\":\"t-1\",\"user_id\":\"u-1\"},\"ttl\":\"15m\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateRow

grpcurl -plaintext \
  -d '{"collection":"sessions","id":1,"payloadJson":"{\"ttl\":\"30m\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/PatchEntity
```

### Query & Search

| RPC | Request | Description |
|:----|:--------|:------------|
| `Query` | `QueryRequest` | Execute SQL/universal query |
| `ExplainQuery` | `QueryRequest` | Get execution plan |
| `Scan` | `ScanRequest` | Paginate through a collection |
| `Search` | `JsonPayloadRequest` | Unified search (`mode=auto|index|multimodal|hybrid`) |
| `TextSearch` | `JsonPayloadRequest` | Full-text search |
| `MultimodalSearch` | `JsonPayloadRequest` | Global multimodal lookup across all structures |
| `HybridSearch` | `JsonPayloadRequest` | Combined text + vector search |
| `Similar` | `JsonCreateRequest` | Vector similarity search |
| `IvfSearch` | `JsonCreateRequest` | IVF approximate search |
| `ContextSearch` | `JsonPayloadRequest` | Context search across all data structures |

### Graph Analytics

| RPC | Request | Description |
|:----|:--------|:------------|
| `GraphNeighborhood` | `JsonPayloadRequest` | Get neighbors of a node |
| `GraphTraverse` | `JsonPayloadRequest` | BFS/DFS traversal |
| `GraphShortestPath` | `JsonPayloadRequest` | Find shortest path |
| `GraphComponents` | `JsonPayloadRequest` | Connected components |
| `GraphCentrality` | `JsonPayloadRequest` | Centrality scores |
| `GraphCommunity` | `JsonPayloadRequest` | Community detection |
| `GraphClustering` | `JsonPayloadRequest` | Clustering coefficient |
| `GraphPersonalizedPagerank` | `JsonPayloadRequest` | Personalized PageRank |
| `GraphHits` | `JsonPayloadRequest` | HITS (hubs/authorities) |
| `GraphCycles` | `JsonPayloadRequest` | Cycle detection |
| `GraphTopologicalSort` | `JsonPayloadRequest` | Topological ordering |

### Graph Projections

| RPC | Request | Description |
|:----|:--------|:------------|
| `GraphProjections` | `Empty` | List all projections |
| `SaveGraphProjection` | `GraphProjectionUpsertRequest` | Create/update projection |
| `DeclaredGraphProjections` | `Empty` | List declared projections |
| `OperationalGraphProjections` | `Empty` | List operational projections |
| `GraphProjectionStatuses` | `Empty` | Projection artifact statuses |
| `GraphProjectionAttention` | `Empty` | Projections needing attention |
| `MaterializeGraphProjection` | `IndexNameRequest` | Materialize a projection |
| `MarkGraphProjectionMaterializing` | `IndexNameRequest` | Mark as materializing |
| `MarkGraphProjectionStale` | `IndexNameRequest` | Mark as stale |
| `FailGraphProjection` | `IndexNameRequest` | Mark as failed |

### Analytics Jobs

| RPC | Request | Description |
|:----|:--------|:------------|
| `AnalyticsJobs` | `Empty` | List all analytics jobs |
| `DeclaredAnalyticsJobs` | `Empty` | List declared jobs |
| `OperationalAnalyticsJobs` | `Empty` | List operational jobs |
| `AnalyticsJobStatuses` | `Empty` | Job artifact statuses |
| `AnalyticsJobAttention` | `Empty` | Jobs needing attention |
| `SaveAnalyticsJob` | `JsonPayloadRequest` | Create/update job |
| `QueueAnalyticsJob` | `JsonPayloadRequest` | Queue job for execution |
| `StartAnalyticsJob` | `JsonPayloadRequest` | Mark job as started |
| `CompleteAnalyticsJob` | `JsonPayloadRequest` | Mark job as completed |
| `MarkAnalyticsJobStale` | `JsonPayloadRequest` | Mark job as stale |
| `FailAnalyticsJob` | `JsonPayloadRequest` | Mark job as failed |

### Index Management

| RPC | Request | Description |
|:----|:--------|:------------|
| `Indexes` | `CollectionRequest` | List indexes for a collection |
| `DeclaredIndexes` | `CollectionRequest` | Declared index definitions |
| `OperationalIndexes` | `CollectionRequest` | Operational indexes |
| `IndexStatuses` | `Empty` | All index statuses |
| `IndexAttention` | `Empty` | Indexes needing attention |
| `SetIndexEnabled` | `IndexToggleRequest` | Enable/disable an index |
| `MarkIndexBuilding` | `IndexNameRequest` | Mark as building |
| `MarkIndexReady` | `IndexNameRequest` | Mark as ready |
| `FailIndex` | `IndexNameRequest` | Mark as failed |
| `MarkIndexStale` | `IndexNameRequest` | Mark as stale |
| `WarmupIndex` | `IndexNameRequest` | Warm up an index |
| `RebuildIndexes` | `CollectionRequest` | Rebuild all indexes |

### Physical Layer & Operations

| RPC | Request | Description |
|:----|:--------|:------------|
| `PhysicalMetadata` | `Empty` | Physical storage metadata |
| `PhysicalAuthority` | `Empty` | Physical authority status |
| `NativeHeader` | `Empty` | Database file header |
| `NativeCollectionRoots` | `Empty` | Collection root pages |
| `NativeManifestSummary` | `Empty` | Manifest summary |
| `NativeRegistrySummary` | `Empty` | Registry summary |
| `NativeRecoverySummary` | `Empty` | Recovery state |
| `NativeCatalogSummary` | `Empty` | Catalog summary |
| `NativeMetadataStateSummary` | `Empty` | Metadata state |
| `NativePhysicalState` | `Empty` | Full physical state |
| `NativeVectorArtifacts` | `Empty` | Vector artifact summary |
| `InspectNativeVectorArtifacts` | `Empty` | Inspect all vector artifacts |
| `InspectNativeVectorArtifact` | `CollectionRequest` | Inspect specific artifact |
| `NativeHeaderRepairPolicy` | `Empty` | Header repair policy |
| `RepairNativeHeader` | `Empty` | Repair database header |
| `WarmupNativeVectorArtifacts` | `Empty` | Warm up all vector artifacts |
| `WarmupNativeVectorArtifact` | `CollectionRequest` | Warm up specific artifact |
| `RepairNativePhysicalState` | `Empty` | Repair physical state |
| `RebuildPhysicalMetadata` | `Empty` | Rebuild physical metadata |

### Snapshots & Exports

| RPC | Request | Description |
|:----|:--------|:------------|
| `Manifest` | `ManifestRequest` | Get manifest (optionally since snapshot) |
| `Roots` | `Empty` | Get collection roots |
| `Snapshots` | `Empty` | List snapshots |
| `Exports` | `Empty` | List exports |
| `CreateSnapshot` | `Empty` | Create a new snapshot |
| `CreateExport` | `ExportRequest` | Create a named export |
| `ApplyRetention` | `Empty` | Apply retention policy |
| `Checkpoint` | `Empty` | Force checkpoint |

### Replication

| RPC | Request | Description |
|:----|:--------|:------------|
| `ReplicationStatus` | `Empty` | Get replication status |
| `PullWalRecords` | `JsonPayloadRequest` | Pull WAL records from primary |
| `ReplicationSnapshot` | `Empty` | Get replication snapshot |

### DDL

| RPC | Request | Description |
|:----|:--------|:------------|
| `CreateCollection` | `JsonPayloadRequest` | Create a collection |
| `DropCollection` | `JsonPayloadRequest` | Drop a collection |
| `DescribeCollection` | `CollectionRequest` | Describe collection schema |

`CreateCollection` accepts `ttl` or `ttl_ms` in `payloadJson` as the default collection TTL:

```bash
grpcurl -plaintext \
  -d '{"payloadJson":"{\"name\":\"sessions\",\"ttl\":\"60m\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateCollection
```

`DescribeCollection` returns `default_ttl_ms` and `default_ttl` when configured.

### Auth

| RPC | Request | Description |
|:----|:--------|:------------|
| `AuthBootstrap` | `JsonPayloadRequest` | Bootstrap first admin user |
| `AuthLogin` | `JsonPayloadRequest` | Login and get session token |
| `AuthCreateUser` | `JsonPayloadRequest` | Create a user |
| `AuthDeleteUser` | `JsonPayloadRequest` | Delete a user |
| `AuthListUsers` | `Empty` | List all users |
| `AuthCreateApiKey` | `JsonPayloadRequest` | Create an API key |
| `AuthRevokeApiKey` | `JsonPayloadRequest` | Revoke an API key |
| `AuthChangePassword` | `JsonPayloadRequest` | Change password |
| `AuthWhoAmI` | `Empty` | Get current user info |

### AI

| RPC | Request | Response | Description |
|:----|:--------|:---------|:------------|
| `Ask` | `JsonPayloadRequest` | `PayloadReply` | RAG query: search context + LLM answer |
| `Embeddings` | `JsonPayloadRequest` | `PayloadReply` | Generate AI embeddings |
| `AiPrompt` | `JsonPayloadRequest` | `PayloadReply` | Execute AI prompt |
| `AiCredentials` | `JsonPayloadRequest` | `PayloadReply` | Configure AI provider credentials and base URLs |

All AI RPCs accept a `provider` field: `openai`, `anthropic`, `groq`, `openrouter`, `together`, `venice`, `deepseek`, `ollama`, `huggingface`, `local`, or a custom URL.

> [!NOTE]
> Configuration export and import (the `/config` endpoints) are available via the HTTP API only. Use `GET /config` to export and `POST /config` to import configuration as nested JSON. See the [HTTP API docs](http.md#configuration) for details.

### Serverless

| RPC | Request | Description |
|:----|:--------|:------------|
| `ServerlessAttach` | `JsonPayloadRequest` | Attach serverless instance |
| `ServerlessWarmup` | `JsonPayloadRequest` | Warm up serverless instance |
| `ServerlessReclaim` | `JsonPayloadRequest` | Reclaim serverless resources |

## Message Types

### Key Request Messages

```protobuf
message QueryRequest {
  string query = 1;
  repeated string entity_types = 2;
  repeated string capabilities = 3;
}

message JsonCreateRequest {
  string collection = 1;
  string payload_json = 2;
}

message ScanRequest {
  string collection = 1;
  uint64 offset = 2;
  uint64 limit = 3;
}

message UpdateEntityRequest {
  string collection = 1;
  uint64 id = 2;
  string payload_json = 3;
}

message DeleteEntityRequest {
  string collection = 1;
  uint64 id = 2;
}
```

### Key Response Messages

```protobuf
message QueryReply {
  bool ok = 1;
  string mode = 2;
  string statement = 3;
  string engine = 4;
  repeated string columns = 5;
  uint64 record_count = 6;
  string result_json = 7;
}

message EntityReply {
  bool ok = 1;
  uint64 id = 2;
  string entity_json = 3;
}

message StatsReply {
  uint64 collection_count = 1;
  uint64 total_entities = 2;
  uint64 total_memory_bytes = 3;
  uint64 cross_ref_count = 4;
  uint64 active_connections = 5;
  uint64 idle_connections = 6;
  uint64 total_checkouts = 7;
  bool paged_mode = 8;
  uint64 started_at_unix_ms = 9;
}
```
