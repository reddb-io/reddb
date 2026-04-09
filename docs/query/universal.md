# Universal Query (FROM ANY)

The universal query is one of RedDB's most powerful features. It searches across **all entity types and all collections** in a single query, returning results in a unified envelope.

## Syntax

```sql
FROM ANY [WHERE condition] [ORDER BY field [ASC|DESC]] [LIMIT n] [OFFSET n]
```

## Basic Usage

### All Entities

```sql
FROM ANY ORDER BY _score DESC LIMIT 10
```

### Filter by Entity Kind

```sql
FROM ANY WHERE _kind = 'row' LIMIT 50
```

```sql
FROM ANY WHERE _kind = 'node' OR _kind = 'edge' LIMIT 50
```

### Filter by Collection

```sql
FROM ANY WHERE _collection = 'users' LIMIT 20
```

### Combined Filters

```sql
FROM ANY WHERE _kind = 'row' AND _collection = 'hosts' ORDER BY _entity_id DESC LIMIT 10
```

## Entity Kinds

| Kind | Description |
|:-----|:------------|
| `row` | Table row |
| `node` | Graph node |
| `edge` | Graph edge |
| `vector` | Vector embedding |
| `document` | JSON document |
| `kv` | Key-value pair |

## Unified Envelope

Every entity returned by a universal query includes standard fields:

```json
{
  "_entity_id": 42,
  "_collection": "hosts",
  "_kind": "row",
  "_entity_type": "row",
  "_capabilities": ["read", "write", "delete"],
  "_score": 1.0,
  "ip": "10.0.0.1",
  "os": "linux"
}
```

| Field | Type | Description |
|:------|:-----|:------------|
| `_entity_id` | `u64` | Unique entity identifier |
| `_collection` | `string` | Source collection name |
| `_kind` | `string` | Entity kind (row, node, edge, vector, document, kv) |
| `_entity_type` | `string` | Entity type classification |
| `_capabilities` | `string[]` | Operations supported on this entity |
| `_score` | `f64` | Relevance score |

## Example: Cross-Model Query

This is the power of universal queries. In a single request, you can retrieve a host row, its graph node representation, and its vector embedding:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "FROM ANY ORDER BY _score DESC LIMIT 20"}'
```

Response:

```json
{
  "ok": true,
  "mode": "sql",
  "engine": "universal",
  "record_count": 3,
  "records": [
    {
      "_entity_id": 1,
      "_collection": "hosts",
      "_kind": "row",
      "_score": 1.0,
      "ip": "10.0.0.1",
      "os": "linux"
    },
    {
      "_entity_id": 2,
      "_collection": "network",
      "_kind": "node",
      "_score": 0.95,
      "label": "web-server-01",
      "node_type": "host"
    },
    {
      "_entity_id": 3,
      "_collection": "embeddings",
      "_kind": "vector",
      "_score": 0.88,
      "content": "host 10.0.0.1 running ssh"
    }
  ]
}
```

## Filtering with Entity Types and Capabilities

The gRPC `Query` RPC accepts additional filters:

```bash
grpcurl -plaintext \
  -d '{
    "query": "FROM ANY LIMIT 20",
    "entity_types": ["row", "node"],
    "capabilities": ["read"]
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/Query
```

## Query Flow

```mermaid
flowchart LR
    A[FROM ANY] --> B[Scan All Collections]
    B --> C[Apply Filters]
    C --> D[Score & Rank]
    D --> E[Sort + Paginate]
    E --> F[Unified Envelope]
```

> [!WARNING]
> Universal queries scan all collections. For large databases, always use `LIMIT` to bound the result set. Target specific collections with `WHERE _collection = '...'` when possible.
