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

## WITH Clauses

You can attach expiration and metadata to inserted entities using `WITH` clauses. These are the structured alternative to the old approach of setting `_ttl` or `_ttl_ms` as regular columns.

### Syntax

```sql
INSERT INTO table_name (columns) VALUES (values) [WITH TTL duration] [WITH EXPIRES AT timestamp] [WITH METADATA (key = 'value', ...)] [WITH AUTO EMBED (fields) USING provider]
```

### WITH TTL

Sets a relative expiration on the entity. After the specified duration, RedDB automatically removes the entity. Supported units: `ms` (milliseconds), `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

```sql
INSERT INTO sessions (token, user_id) VALUES ('abc', 42) WITH TTL 1 h
INSERT INTO cache (key, value) VALUES ('theme', 'dark') WITH TTL 30 m
```

### WITH EXPIRES AT

Sets an absolute expiration using a Unix timestamp in milliseconds. The entity is removed when the system clock passes this timestamp.

```sql
INSERT INTO events (name) VALUES ('launch') WITH EXPIRES AT 1735689600000
```

### WITH METADATA

Attaches structured key-value metadata to the entity. Metadata is stored separately from the entity fields and can be used for filtering, auditing, or routing.

```sql
INSERT INTO events (name) VALUES ('login') WITH METADATA (priority = 'high', source = 'web')
```

### Combining WITH Clauses

You can chain multiple `WITH` clauses on a single statement:

```sql
INSERT INTO sessions (token) VALUES ('abc') WITH TTL 1 h WITH METADATA (source = 'mobile')
```

### WITH AUTO EMBED

Automatically generates a vector embedding for one or more text fields at insert time. RedDB sends the field value to the configured provider, stores the resulting vector alongside the entity, and makes it available for similarity search.

```sql
-- Auto-embed with different providers
INSERT INTO articles (title, body) VALUES ('AI', 'Long text...')
  WITH AUTO EMBED (body) USING openai

INSERT INTO docs (content) VALUES ('Security report...')
  WITH AUTO EMBED (content) USING ollama MODEL 'nomic-embed-text'
```

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `(field, ...)` | Yes | Fields whose values are embedded |
| `USING provider` | No | Embedding provider (`openai`, `groq`, `ollama`, `anthropic`) |
| `MODEL 'name'` | No | Specific embedding model (provider-dependent) |

You can combine `WITH AUTO EMBED` with other `WITH` clauses:

```sql
INSERT INTO logs (message) VALUES ('Unusual traffic spike')
  WITH AUTO EMBED (message) USING openai
  WITH TTL 30 d
  WITH METADATA (severity = 'high')
```

> [!TIP]
> Prefer `WITH TTL` and `WITH EXPIRES AT` over setting `_ttl` or `_ttl_ms` as column values. The `WITH` syntax is clearer, validated at parse time, and keeps expiration concerns separate from your data fields.

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
