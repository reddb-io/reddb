# INSERT

The `INSERT` statement adds new entities to a collection. RedDB supports inserting rows, native
time-series points, nodes, edges, vectors, documents, and KV pairs.

Prefer positional parameters for row values:

```ts
const sql = "INSERT INTO users (name, email, age, active) VALUES ($1, $2, $3, $4)";
const params = ["Alice", "alice@example.com", 30, true];
await db.query(sql, params);
```

The parameterized-query design is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

## SQL Syntax

```sql
INSERT INTO table_name (column1, column2, ...) VALUES (value1, value2, ...)
```

### Insert a Row

```sql
INSERT INTO users (name, email, age, active) VALUES ($1, $2, $3, $4)
```

### Multiple Rows

```sql
INSERT INTO users (name, email, age) VALUES
  ($1, $2, $3),
  ($4, $5, $6)
```

### Insert a Native Time-Series Point

If the target collection was declared with `CREATE TIMESERIES`, a plain `INSERT INTO` writes native
time-series points instead of table rows.

```sql
CREATE TIMESERIES cpu_metrics RETENTION 7 d

INSERT INTO cpu_metrics (metric, value, tags, timestamp)
  VALUES ($1, $2, $3, $4)
```

Supported columns for native time-series inserts:

| Column | Required | Notes |
|:-------|:---------|:------|
| `metric` | Yes | Series name |
| `value` | Yes | Numeric sample |
| `tags` | No | Inline object literal or JSON object text |
| `timestamp` / `timestamp_ns` / `time` | No | Unix timestamp in nanoseconds; omit to auto-generate |

Exactly one timestamp alias may be provided per row.

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
    "from_rid": 102,
    "to_rid": 103,
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

### Bulk Insert with AUTO EMBED

Add `auto_embed` to the bulk rows body to generate embeddings for all rows in one provider batch call. The engine embeds before inserting, so a provider failure leaves the collection untouched.

```bash
curl -X POST http://127.0.0.1:8080/collections/articles/bulk/rows \
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

Response: `{"ok": true, "created_count": 2, "embedded_count": 2, "provider_requests": 1}`

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
INSERT INTO sessions (token, user_id) VALUES ($1, $2) WITH TTL 1 h
INSERT INTO cache (key, value) VALUES ($1, $2) WITH TTL 30 m
```

### WITH EXPIRES AT

Sets an absolute expiration using a Unix timestamp in milliseconds. The entity is removed when the system clock passes this timestamp.

```sql
INSERT INTO events (name) VALUES ($1) WITH EXPIRES AT 1735689600000
```

### WITH METADATA

Attaches structured key-value metadata to the entity. Metadata is stored separately from the entity fields and can be used for filtering, auditing, or routing.

```sql
INSERT INTO events (name) VALUES ($1) WITH METADATA (priority = 'high', source = 'web')
```

### Combining WITH Clauses

You can chain multiple `WITH` clauses on a single statement:

```sql
INSERT INTO sessions (token) VALUES ($1) WITH TTL 1 h WITH METADATA (source = 'mobile')
```

### WITH AUTO EMBED

Automatically generates a vector embedding for one or more text fields at insert time. RedDB sends the field value to the configured provider, stores the resulting vector alongside the entity, and makes it available for similarity search.

```sql
-- Auto-embed with different providers
INSERT INTO articles (title, body) VALUES ($1, $2)
  WITH AUTO EMBED (body) USING openai

INSERT INTO docs (content) VALUES ($1)
  WITH AUTO EMBED (content) USING ollama MODEL 'nomic-embed-text'
```

| Parameter | Required | Description |
|:----------|:---------|:------------|
| `(field, ...)` | Yes | Fields whose values are embedded |
| `USING provider` | No | Embedding provider (`openai`, `groq`, `ollama`, `anthropic`) |
| `MODEL 'name'` | No | Specific embedding model (provider-dependent) |

You can combine `WITH AUTO EMBED` with other `WITH` clauses:

```sql
INSERT INTO logs (message) VALUES ($1)
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
    "rid": 1,
    "collection": "users",
    "kind": "row",
    "tenant": null,
    "created_at": 1760000000000,
    "updated_at": 1760000000000
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
> Collections are created automatically on first insert for generic row-style data. Model-specific
> collections such as `CREATE TIMESERIES` and `CREATE QUEUE` should still be declared explicitly so
> RedDB can enforce the correct native semantics.
