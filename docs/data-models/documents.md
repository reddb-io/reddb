# Documents

Documents store semi-structured JSON payloads with optional metadata. They are ideal for configuration objects, event logs, API responses, and any data that doesn't fit a rigid table schema.

## SQL First

Documents are still queryable with SQL-style reads because RedDB flattens top-level fields from the document body into queryable columns.

Typical flow:

```sql
SELECT * FROM events
```

```sql
SELECT event_type, user_id, timestamp
FROM events
WHERE event_type = 'login'
ORDER BY timestamp DESC
LIMIT 20
```

```sql
SELECT title, category
FROM articles
WHERE published = true
ORDER BY _entity_id DESC
LIMIT 10
```

If you want to search documents across collections, use the universal envelope:

```sql
FROM ANY
WHERE _kind = 'document' AND _collection = 'events'
ORDER BY _entity_id DESC
LIMIT 20
```

## Creating Documents

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:8080/collections/events/documents \
  -H 'content-type: application/json' \
  -d '{
    "body": {
      "event_type": "login",
      "user_id": "u_abc123",
      "timestamp": "2024-01-15T10:30:00Z",
      "details": {
        "ip": "192.168.1.100",
        "user_agent": "Mozilla/5.0",
        "success": true
      }
    },
    "metadata": {
      "source": "auth-service",
      "environment": "production"
    }
  }'
```

#### **gRPC**

```bash
grpcurl -plaintext \
  -d '{
    "collection": "events",
    "payloadJson": "{\"body\":{\"event_type\":\"login\",\"user_id\":\"u_abc123\"},\"metadata\":{\"source\":\"auth-service\"}}"
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateRow
```

#### **MCP (AI Agent)**

```json
{
  "tool": "reddb_insert_document",
  "arguments": {
    "collection": "events",
    "body": {
      "event_type": "login",
      "user_id": "u_abc123",
      "timestamp": "2024-01-15T10:30:00Z"
    },
    "metadata": {
      "source": "auth-service"
    }
  }
}
```

<!-- tabs:end -->

## Document Structure

A document entity consists of:

| Field | Required | Description |
|:------|:---------|:------------|
| `body` | Yes | The JSON document content (any valid JSON object) |
| `metadata` | No | Key-value pairs for classification and filtering |

## Querying Documents

Documents are queryable both as collection-scoped SQL reads and through the universal query engine.

Collection-scoped examples:

```sql
SELECT * FROM events
```

```sql
SELECT event_type, user_id, body
FROM events
WHERE event_type = 'login'
LIMIT 20
```

```sql
SELECT title, index
FROM doc_multi
WHERE index >= 2
ORDER BY index DESC
```

Universal examples:

```sql
FROM ANY WHERE _kind = 'document' AND _collection = 'events' LIMIT 20
```

```sql
FROM ANY
WHERE _kind = 'document'
  AND _collection = 'events'
  AND event_type = 'login'
LIMIT 20
```

This works best when the fields you need are at the top level of the document body.

Via HTTP:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT event_type, user_id, body FROM events WHERE event_type = '\''login'\'' LIMIT 20"}'
```

## Updating Documents

Patch specific fields in a document:

```bash
curl -X PATCH http://127.0.0.1:8080/collections/events/entities/42 \
  -H 'content-type: application/json' \
  -d '{
    "body": {
      "details": {
        "reviewed": true,
        "reviewed_by": "admin"
      }
    }
  }'
```

## Deleting Documents

```bash
curl -X DELETE http://127.0.0.1:8080/collections/events/entities/42
```

## Use Cases

- **Event logs**: Store audit trails, user activity, system events
- **Configuration**: Application config objects that change over time
- **API caching**: Cache external API responses with metadata
- **Content storage**: Blog posts, articles, CMS content with flexible schemas

## Practical SQL Patterns

Query the flattened fields:

```sql
SELECT name, age, active
FROM doc_flatten
WHERE active = true
```

Keep the full JSON body in the result:

```sql
SELECT title, body
FROM articles
WHERE category = 'database'
LIMIT 10
```

> [!TIP]
> Documents and rows can coexist in the same collection. Use `FROM ANY` queries to search across both shapes.
