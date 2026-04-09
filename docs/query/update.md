# UPDATE

The `UPDATE` statement modifies existing rows in a table.

## Syntax

```sql
UPDATE table_name SET column1 = value1 [, column2 = value2, ...] [WHERE condition]
```

## Examples

### Update a Single Field

```sql
UPDATE users SET age = 31 WHERE name = 'Alice'
```

### Update Multiple Fields

```sql
UPDATE hosts SET os = 'ubuntu', critical = false WHERE ip = '10.0.0.2'
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
  -d '{"query": "UPDATE users SET age = 31 WHERE name = '\''Alice'\''"}'
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

## Response

```json
{
  "ok": true,
  "id": 1,
  "entity": {
    "_entity_id": 1,
    "_collection": "users",
    "_kind": "row",
    "name": "Alice",
    "age": 31,
    "active": true
  }
}
```
