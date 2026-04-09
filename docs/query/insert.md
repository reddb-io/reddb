# INSERT

The `INSERT` statement adds new entities to a collection. RedDB supports inserting all entity types: rows, nodes, edges, vectors, documents, and KV pairs.

## SQL Syntax

```sql
INSERT INTO table_name (column1, column2, ...) VALUES (value1, value2, ...)
```

### Insert a Row

```sql
INSERT INTO users (name, email, age, active) VALUES ('Alice', 'alice@example.com', 30, true)
```

### Multiple Rows

```sql
INSERT INTO users (name, email, age) VALUES
  ('Bob', 'bob@example.com', 25),
  ('Charlie', 'charlie@example.com', 35)
```

## API Insert (All Entity Types)

### Row

```bash
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Alice", "email": "alice@example.com", "age": 30}}'
```

### Node

```bash
curl -X POST http://127.0.0.1:8080/collections/graph/nodes \
  -H 'content-type: application/json' \
  -d '{
    "label": "alice",
    "node_type": "person",
    "properties": {"name": "Alice", "dept": "eng"}
  }'
```

### Edge

```bash
curl -X POST http://127.0.0.1:8080/collections/graph/edges \
  -H 'content-type: application/json' \
  -d '{
    "label": "FOLLOWS",
    "from": 1,
    "to": 2,
    "weight": 1.0
  }'
```

### Vector

```bash
curl -X POST http://127.0.0.1:8080/collections/embeddings/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.91, 0.44],
    "content": "vector content text",
    "metadata": {"source": "api"}
  }'
```

### Document

```bash
curl -X POST http://127.0.0.1:8080/collections/logs/documents \
  -H 'content-type: application/json' \
  -d '{
    "body": {"event": "login", "user_id": "u123"},
    "metadata": {"env": "prod"}
  }'
```

## Bulk Insert

For high-throughput ingestion, use the bulk endpoints:

```bash
# Bulk rows
curl -X POST http://127.0.0.1:8080/collections/users/bulk/rows \
  -H 'content-type: application/json' \
  -d '[
    {"fields": {"name": "Alice", "age": 30}},
    {"fields": {"name": "Bob", "age": 25}}
  ]'

# Bulk nodes
curl -X POST http://127.0.0.1:8080/collections/graph/bulk/nodes \
  -H 'content-type: application/json' \
  -d '[
    {"label": "alice", "node_type": "person"},
    {"label": "bob", "node_type": "person"}
  ]'

# Bulk vectors
curl -X POST http://127.0.0.1:8080/collections/embeddings/bulk/vectors \
  -H 'content-type: application/json' \
  -d '[
    {"dense": [0.1, 0.2, 0.3], "content": "Doc A"},
    {"dense": [0.4, 0.5, 0.6], "content": "Doc B"}
  ]'
```

## gRPC Bulk Insert

```bash
grpcurl -plaintext \
  -d '{
    "collection": "users",
    "payloadJson": [
      "{\"fields\":{\"name\":\"Alice\",\"age\":30}}",
      "{\"fields\":{\"name\":\"Bob\",\"age\":25}}"
    ]
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/BulkCreateRows
```

## Response

Single insert:

```json
{
  "ok": true,
  "id": 1,
  "entity": {
    "_entity_id": 1,
    "_collection": "users",
    "_kind": "row"
  }
}
```

Bulk insert:

```json
{
  "ok": true,
  "count": 2,
  "items": [
    {"ok": true, "id": 1},
    {"ok": true, "id": 2}
  ]
}
```

> [!TIP]
> Collections are created automatically on first insert. No explicit `CREATE TABLE` is needed unless you want to define a schema upfront.
