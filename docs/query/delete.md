# DELETE

The `DELETE` statement removes items from a collection. For SQL row deletes,
filter by the public RedDB ID `rid` when deleting one known item.

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
DELETE FROM table_name [WHERE condition] [RETURNING * | field [, field ...]]
```

## Examples

### Delete with Filter

```sql
DELETE FROM users WHERE name = $1
```

```sql
DELETE FROM users WHERE rid = $1 RETURNING rid, name
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

### By RedDB ID

```bash
curl -X DELETE http://127.0.0.1:8080/collections/users/entities/102
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
  -d '{"collection": "users", "id": 102}' \
  127.0.0.1:50051 reddb.v1.RedDb/DeleteEntity
```

`DeleteEntityRequest.id` is the retained protobuf field name. Treat its value
as the public RedDB ID `rid`.

## Via MCP

```json
{
  "tool": "reddb_delete",
  "arguments": {
    "collection": "users",
    "rid": 102
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
