# Documents

Documents store semi-structured JSON payloads with optional metadata. They are ideal for configuration objects, event logs, API responses, and any data that doesn't fit a rigid table schema.

> [!NOTE]
> Documents are a first-class user-facing model, but they do not currently introduce a separate
> native storage entity kind in the core unified engine. They are exposed as document semantics on
> top of the unified collection layer.

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
ORDER BY rid DESC
LIMIT 10
```

If you want to search documents across collections, use the universal envelope:

```sql
FROM ANY
WHERE kind = 'document' AND collection = 'events'
ORDER BY rid DESC
LIMIT 20
```

## Creating a Document Collection

Create the collection before inserting into it. Documents use their own DDL — a
bare collection name, no column list:

```sql
CREATE DOCUMENT events
```

`IF NOT EXISTS` is supported and makes the statement idempotent:

```sql
CREATE DOCUMENT IF NOT EXISTS events
```

> [!WARNING]
> RedDB documents are **schemaless** — you do **not** declare columns, and
> PostgreSQL-style DDL does not apply. The following are **not** supported and
> will fail to parse:
>
> - `CREATE TABLE events DOCUMENT (id UUID PRIMARY KEY, body JSONB NOT NULL, ...)`
>   → use `CREATE DOCUMENT events` instead.
> - Generated columns such as
>   `ALTER TABLE events ADD COLUMN event_type TEXT GENERATED ALWAYS AS (body->>'event_type') STORED`
>   → unnecessary. RedDB automatically flattens top-level fields of each
>   document `body` into queryable columns, so `SELECT event_type FROM events` works
>   with no generated column. See [SQL First](#sql-first) above.

## Creating Documents

Once the collection exists, insert documents with the idempotent assertion form:

```sql
INSERT INTO events DOCUMENT VALUES ({"event_type":"login","user_id":"u_abc123"})
```

The HTTP, gRPC, and MCP surfaces below are equivalent.

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:5000/collections/events/documents \
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
  127.0.0.1:55055 reddb.v1.RedDb/CreateRow
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

Returned document items also include the public envelope fields `rid`,
`collection`, `kind`, `tenant`, `created_at`, and `updated_at`, with
`kind = 'document'`. Those envelope names are reserved; top-level document body
fields cannot use them.

## Reserved field names

The public RedDB item envelope reserves these top-level names in user data:
`rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
Document writes that include one of those names as a top-level `body` field are
rejected at write time.

If your source JSON uses one of those names, rename that field before inserting
it, for example from `kind` to `event_type`. This is the permanent collision
rule from [ADR 0066](../../.red/adr/0066-reserved-envelope-fields-user-pays.md):
the envelope vocabulary stays unprefixed and user data pays for top-level
collisions.

## Identifier and data case rule

SQL identifiers (collection names, column references in `WHERE` and `SET`)
fold case — `UserId` and `userid` refer to the same top-level field. JSON
body keys are user data and are **matched exactly**:

```sql
INSERT INTO logs DOCUMENT VALUES ({"UserId":"u_123","event":"login"})
```

This document has two top-level fields: `UserId` and `event`. A query using
a lowercase identifier still matches the field:

```sql
SELECT userid FROM logs
```

returns `u_123`. However, a query that uses exact JSON-key syntax:

```sql
SELECT body->>'userid' FROM logs
```

returns `NULL` because the key `userid` does not exist — the stored key is
`UserId` with uppercase U. This asymmetry is by design per
[ADR 0067](../../.red/adr/0067-document-dml-surface-clean-break.md): RedDB
flattens top-level body keys into queryable columns with SQL's case-insensitive
identifier semantics, but the underlying JSON keys are never case-folded.
The schema-free document model places the responsibility on the user to avoid
typos such as `userid` vs `UserId` — an empty result silently alerts you to
a case mismatch.

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
FROM ANY WHERE kind = 'document' AND collection = 'events' LIMIT 20
```

```sql
FROM ANY
WHERE kind = 'document'
  AND collection = 'events'
  AND event_type = 'login'
LIMIT 20
```

This works best when the fields you need are at the top level of the document body.

Via HTTP:

```bash
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT event_type, user_id, body FROM events WHERE event_type = '\''login'\'' LIMIT 20"}'
```

## Updating Documents

Patch specific nested fields in a document with JSON-pointer-style paths under `body`:

```bash
curl -X PATCH http://127.0.0.1:5000/collections/events/entities/102 \
  -H 'content-type: application/json' \
  -d '{
    "operations": [
      { "op": "set", "path": "/body/details/reviewed", "value": true },
      { "op": "set", "path": "/body/details/reviewed_by", "value": "admin" },
      { "op": "unset", "path": "/body/details/queued_for_review" }
    ]
  }'
```

`set` creates missing intermediate objects. `unset` on an absent field is a no-op.
Array positional paths such as `/body/tags/0` are not supported; replace the array or the full
document body instead.

Add `"dry_run": true` to the body to validate operations without mutating; the
response is `{ok:true, dry_run:true, operations:N}`. Validation failures
return a structured envelope with `code`, `op_index`, and a JSON Pointer
`pointer` so an editor UI can highlight the failing field. See
[HTTP API › JSON Patch & path helpers](../api/http.md#json-patch--path-helpers).

### SQL UPDATE on documents

Update documents with standard `UPDATE` syntax. Compound assignment,
`RETURNING`, `LIMIT`, and `ORDER BY ... LIMIT` work the same as for rows:

```sql
UPDATE events
SET retries += 1
WHERE event_type = 'login' AND retries < 5
RETURNING rid, retries
```

```sql
UPDATE events
SET attempts += 1, last_seen = NOW()
WHERE status = 'pending'
ORDER BY created_at ASC
LIMIT 100
RETURNING rid
```

`WHERE` and `SET` see top-level body fields plus the public envelope. Nested
paths (e.g. `body.details.x`) are not part of the first multi-model update
version — use the JSON-patch endpoint above for those. Compound assignment
requires an existing, non-null numeric field; missing, null, non-numeric, or
arithmetic errors abort the whole statement.

Full document body replacement remains available through `body` without `operations`:

```bash
curl -X PATCH http://127.0.0.1:5000/collections/events/entities/102 \
  -H 'content-type: application/json' \
  -d '{
    "body": {
      "event_type": "reviewed",
      "details": {
        "reviewed": true,
        "reviewed_by": "admin"
      }
    }
  }'
```

## Deleting Documents

```bash
curl -X DELETE http://127.0.0.1:5000/collections/events/entities/102
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
