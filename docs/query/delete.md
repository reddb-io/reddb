# DELETE

The `DELETE` statement removes rows from a table.

Prefer positional parameters for runtime values:

```ts
const sql = "DELETE FROM users WHERE name = $1";
const params = ["Alice"];
await db.query(sql, params);
```

The parameterized-query design is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

## Syntax

```sql
DELETE FROM table_name [WHERE condition]
```

## Examples

### Delete with Filter

```sql
DELETE FROM users WHERE name = $1
```

### Delete by Condition

```sql
DELETE FROM sessions WHERE expired = $1
```

### Delete All Rows

```sql
DELETE FROM temp_data
```

> [!WARNING]
> Without a `WHERE` clause, all rows in the table are deleted.

## Via HTTP

### By Entity ID

```bash
curl -X DELETE http://127.0.0.1:8080/collections/users/entities/1
```

### Via Query

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "DELETE FROM users WHERE active = $1", "params": [false]}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{"collection": "users", "id": 1}' \
  127.0.0.1:50051 reddb.v1.RedDb/DeleteEntity
```

## Via MCP

```json
{
  "tool": "reddb_delete",
  "arguments": {
    "collection": "users",
    "id": 1
  }
}
```

## Response

```json
{
  "ok": true,
  "message": "entity deleted"
}
```
