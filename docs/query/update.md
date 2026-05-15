# UPDATE

The `UPDATE` statement modifies existing rows in a table.

Prefer positional parameters for values in `SET` and `WHERE`:

```ts
const sql = "UPDATE users SET age = $1 WHERE name = $2";
const params = [31, "Alice"];
await db.query(sql, params);
```

The parameterized-query design is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

## Syntax

```sql
UPDATE table_name SET column1 = value1 [, column2 = value2, ...] [WHERE condition]
```

## Examples

### Update a Single Field

```sql
UPDATE users SET age = $1 WHERE name = $2
```

### Update Multiple Fields

```sql
UPDATE hosts SET os = $1, critical = $2 WHERE ip = $3
```

### Update All Rows

```sql
UPDATE users SET active = false
```

> [!WARNING]
> Without a `WHERE` clause, all rows in the table are updated.

## Via HTTP

### PATCH (by entity ID)

```bash
curl -X PATCH http://127.0.0.1:8080/collections/users/entities/1 \
  -H 'content-type: application/json' \
  -d '{"fields": {"age": 31, "active": true}}'
```

### SQL UPDATE via Query

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "UPDATE users SET age = $1 WHERE name = $2", "params": [31, "Alice"]}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{
    "collection": "users",
    "id": 1,
    "payloadJson": "{\"fields\":{\"age\":31}}"
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/PatchEntity
```

## Via MCP

```json
{
  "tool": "reddb_update",
  "arguments": {
    "collection": "users",
    "set": {"age": 31, "active": true},
    "where_filter": "name = 'Alice'"
  }
}
```

## WITH Clauses

You can attach or replace expiration and metadata on existing entities using `WITH` clauses. These are the structured alternative to the old approach of setting `_ttl` or `_ttl_ms` as regular columns.

### Syntax

```sql
UPDATE table_name SET column = value [WHERE condition] [WITH TTL duration] [WITH EXPIRES AT timestamp] [WITH METADATA (key = 'value', ...)]
```

### WITH TTL

Sets or resets a relative expiration on the matched entities. Supported units: `ms` (milliseconds), `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

```sql
UPDATE sessions SET active = $1 WHERE id = $2 WITH TTL 2 h
```

### WITH EXPIRES AT

Sets an absolute expiration using a Unix timestamp in milliseconds. The entity is removed when the system clock passes this timestamp.

```sql
UPDATE cache SET value = $1 WHERE name = $2 WITH EXPIRES AT 1735689600000
```

### WITH METADATA

Attaches or replaces structured key-value metadata on the matched entities.

```sql
UPDATE users SET name = $1 WHERE id = $2 WITH METADATA (role = 'admin')
```

> [!TIP]
> Prefer `WITH TTL` and `WITH EXPIRES AT` over setting `_ttl` or `_ttl_ms` as column values. The `WITH` syntax is clearer, validated at parse time, and keeps expiration concerns separate from your data fields.

## Response

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
    "updated_at": 1760000001000,
    "name": "Alice",
    "age": 31,
    "active": true
  }
}
```
