# Documents

Documents store semi-structured JSON payloads with optional metadata. They are ideal for configuration objects, event logs, API responses, and any data that doesn't fit a rigid table schema.

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

Documents are queryable through the universal query engine:

```sql
FROM ANY WHERE _kind = 'document' AND _collection = 'events' LIMIT 20
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

> [!TIP]
> Documents and rows can coexist in the same collection. Use `FROM ANY` queries to search across both shapes.
