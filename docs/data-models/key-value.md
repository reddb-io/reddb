# Key-Value

The key-value interface provides fast, direct access to data by key. It is ideal for caches, feature flags, session storage, and any lookup-by-name pattern.

## SQL First

Even though KV has a direct key-based API, the underlying data is still queryable.

Think of a KV collection as a queryable set of `key` / `value` records:

```sql
SELECT * FROM config
```

```sql
SELECT key, value
FROM config
WHERE key = 'max_retries'
```

```sql
SELECT key, value
FROM config
WHERE key LIKE 'feature.%'
ORDER BY key
```

Across collections, you can also use the universal envelope:

```sql
FROM ANY
WHERE _kind = 'kv' AND _collection = 'config'
ORDER BY key
LIMIT 50
```

## Setting a Value

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:8080/collections/config/kv \
  -H 'content-type: application/json' \
  -d '{
    "key": "max_retries",
    "value": 5,
    "metadata": {
      "updated_by": "admin"
    }
  }'
```

#### **MCP (AI Agent)**

```json
{
  "tool": "reddb_kv_set",
  "arguments": {
    "collection": "config",
    "key": "max_retries",
    "value": 5,
    "metadata": {"updated_by": "admin"}
  }
}
```

<!-- tabs:end -->

## Getting a Value

<!-- tabs:start -->

#### **HTTP**

```bash
curl "http://127.0.0.1:8080/collections/config/kv/max_retries"
```

Response:

```json
{
  "ok": true,
  "key": "max_retries",
  "value": 5,
  "metadata": {
    "updated_by": "admin"
  }
}
```

#### **MCP (AI Agent)**

```json
{
  "tool": "reddb_kv_get",
  "arguments": {
    "collection": "config",
    "key": "max_retries"
  }
}
```

<!-- tabs:end -->

## Querying KV with SQL

Direct lookup:

```sql
SELECT value
FROM config
WHERE key = 'max_retries'
```

List a namespace:

```sql
SELECT key, value
FROM config
WHERE key LIKE 'feature.%'
ORDER BY key
```

Inspect everything in the collection:

```sql
SELECT * FROM config ORDER BY key
```

Universal query form:

```sql
FROM ANY
WHERE _kind = 'kv'
  AND _collection = 'config'
LIMIT 100
```

## Deleting a Key

```bash
curl -X DELETE http://127.0.0.1:8080/collections/config/kv/max_retries
```

Delete through the query engine:

```sql
DELETE FROM config WHERE key = 'max_retries'
```

## Value Types

KV values can be any JSON-compatible type:

| Type | Example |
|:-----|:--------|
| String | `"hello world"` |
| Number | `42`, `3.14` |
| Boolean | `true`, `false` |
| Null | `null` |
| Object | `{"nested": "data"}` |
| Array | `[1, 2, 3]` |

## Use Cases

| Pattern | Example |
|:--------|:--------|
| Feature flags | `{"key": "dark_mode_enabled", "value": true}` |
| Session data | `{"key": "session:abc123", "value": {"user_id": 42, "role": "admin"}}` |
| Configuration | `{"key": "rate_limit", "value": 1000}` |
| Counters | `{"key": "page_views", "value": 98432}` |
| Cache entries | `{"key": "user:42:profile", "value": {"name": "Alice", "cached_at": "..."}}` |

> [!NOTE]
> KV pairs are stored as entities in the collection and participate in universal queries. You can mix KV entries with rows, nodes, and vectors in the same collection.
