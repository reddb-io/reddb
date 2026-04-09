# Quick Start

This guide walks you through storing and querying multi-model data in RedDB in under 5 minutes.

## 1. Start the Server

```bash
mkdir -p ./data
red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

> [!TIP]
> Drop `--path` for a purely in-memory database that requires no disk.

## 2. Create a Table Row

Insert a structured row into the `hosts` collection:

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "ip": "10.0.0.1",
      "os": "linux",
      "critical": true
    }
  }'
```

Response:

```json
{
  "ok": true,
  "id": 1,
  "entity": {
    "_entity_id": 1,
    "_collection": "hosts",
    "_kind": "row",
    "ip": "10.0.0.1",
    "os": "linux",
    "critical": true
  }
}
```

## 3. Create a Graph Node

```bash
curl -X POST http://127.0.0.1:8080/collections/network/nodes \
  -H 'content-type: application/json' \
  -d '{
    "label": "web-server-01",
    "node_type": "host",
    "properties": {
      "ip": "10.0.0.1",
      "datacenter": "us-east"
    }
  }'
```

## 4. Create a Graph Edge

Link two nodes with a relationship:

```bash
curl -X POST http://127.0.0.1:8080/collections/network/edges \
  -H 'content-type: application/json' \
  -d '{
    "label": "CONNECTS_TO",
    "from": 1,
    "to": 2,
    "weight": 1.0,
    "properties": {
      "protocol": "tcp",
      "port": 443
    }
  }'
```

## 5. Insert a Vector Embedding

```bash
curl -X POST http://127.0.0.1:8080/collections/embeddings/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.91, 0.44, 0.33, 0.67],
    "content": "web server running nginx on port 443",
    "metadata": {
      "source": "scan-2024-01"
    }
  }'
```

## 6. Query with SQL

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM hosts WHERE critical = true"}'
```

Response envelope:

```json
{
  "ok": true,
  "mode": "sql",
  "engine": "table",
  "columns": ["_entity_id", "_collection", "_kind", "ip", "os", "critical"],
  "record_count": 1,
  "records": [
    {
      "_entity_id": 1,
      "_collection": "hosts",
      "_kind": "row",
      "ip": "10.0.0.1",
      "os": "linux",
      "critical": true
    }
  ]
}
```

## 7. Universal Query (FROM ANY)

Query across all entity types at once:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "FROM ANY ORDER BY _score DESC LIMIT 10"}'
```

This returns rows, nodes, edges, and vectors from all collections in a single result set.

## 8. Check Health

```bash
curl -s http://127.0.0.1:8080/health | python3 -m json.tool
```

```json
{
  "healthy": true,
  "state": "running",
  "checked_at": "2024-01-15T10:30:00Z"
}
```

## 9. Bulk Insert

Insert many rows at once for better throughput:

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/bulk/rows \
  -H 'content-type: application/json' \
  -d '[
    {"fields": {"ip": "10.0.0.2", "os": "windows", "critical": false}},
    {"fields": {"ip": "10.0.0.3", "os": "linux", "critical": true}},
    {"fields": {"ip": "10.0.0.4", "os": "macos", "critical": false}}
  ]'
```

## What's Next?

- [Data Models](/data-models/tables.md) -- Learn about each entity type in depth
- [Query Language](/query/select.md) -- Full SELECT syntax and operators
- [gRPC API](/api/grpc.md) -- All 116 RPC endpoints
- [Configuration](/getting-started/configuration.md) -- Server flags, env vars, and storage modes
