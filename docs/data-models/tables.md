# Tables & Rows

Tables are the structured, relational face of RedDB. Each row is an entity with typed fields, stored in a named collection.

## SQL First

If you are thinking in SQL, tables are the most direct RedDB model.

Typical flow:

```sql
CREATE TABLE users (
  name Text NOT NULL,
  email Text,
  age Integer,
  active Boolean DEFAULT true
)

INSERT INTO users (name, email, age, active)
VALUES ('Alice', 'alice@example.com', 30, true)

SELECT name, email, age
FROM users
WHERE age >= 21 AND active = true
ORDER BY name
LIMIT 10

UPDATE users
SET age = 31
WHERE name = 'Alice'

DELETE FROM users
WHERE active = false
```

## Creating Rows

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "name": "Alice",
      "email": "alice@example.com",
      "age": 30,
      "active": true
    }
  }'
```

#### **gRPC**

```bash
grpcurl -plaintext \
  -d '{
    "collection": "users",
    "payloadJson": "{\"fields\":{\"name\":\"Alice\",\"email\":\"alice@example.com\",\"age\":30,\"active\":true}}"
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateRow
```

#### **Rust (Embedded)**

```rust
let id = db.row("users", vec![
    ("name", Value::Text("Alice".into())),
    ("email", Value::Text("alice@example.com".into())),
    ("age", Value::Integer(30)),
    ("active", Value::Boolean(true)),
]).save()?;
```

<!-- tabs:end -->

## Querying Rows

Use SQL-like syntax to query rows:

```sql
SELECT * FROM users WHERE age > 21 AND active = true ORDER BY name LIMIT 10
```

More examples:

```sql
SELECT name, email FROM users
```

```sql
SELECT * FROM users WHERE email IS NOT NULL ORDER BY age DESC LIMIT 20
```

```sql
SELECT * FROM users WHERE name LIKE '%ali%'
```

```sql
SELECT * FROM users WHERE age BETWEEN 18 AND 65
```

Via HTTP:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT name, email FROM users WHERE age > 21 ORDER BY name"}'
```

## Updating Rows

```bash
curl -X PATCH http://127.0.0.1:8080/collections/users/entities/1 \
  -H 'content-type: application/json' \
  -d '{"fields": {"age": 31}}'
```

Or via SQL:

```sql
UPDATE users SET age = 31 WHERE name = 'Alice'
```

```sql
UPDATE users
SET active = false, age = 32
WHERE email = 'alice@example.com'
```

## Deleting Rows

```bash
curl -X DELETE http://127.0.0.1:8080/collections/users/entities/1
```

Or via SQL:

```sql
DELETE FROM users WHERE name = 'Alice'
```

```sql
DELETE FROM users WHERE active = false
```

## Creating Tables with DDL

If you want schema upfront instead of implicit collection creation:

```sql
CREATE TABLE hosts (
  ip IpAddr NOT NULL,
  hostname Text NOT NULL,
  os Text,
  critical Boolean DEFAULT false,
  last_seen Timestamp
)
```

```sql
CREATE TABLE sessions (
  token Text NOT NULL,
  user_id Text NOT NULL
) WITH TTL 60m
```

## Bulk Insert

Insert many rows in a single request for better throughput:

```bash
curl -X POST http://127.0.0.1:8080/collections/users/bulk/rows \
  -H 'content-type: application/json' \
  -d '[
    {"fields": {"name": "Bob", "email": "bob@example.com", "age": 25}},
    {"fields": {"name": "Charlie", "email": "charlie@example.com", "age": 35}},
    {"fields": {"name": "Diana", "email": "diana@example.com", "age": 28}}
  ]'
```

## Scanning

Paginate through all rows in a collection:

```bash
curl "http://127.0.0.1:8080/collections/users/scan?offset=0&limit=50"
```

If you prefer staying in SQL:

```sql
SELECT * FROM users ORDER BY _entity_id ASC LIMIT 50 OFFSET 0
```

## Row Envelope

Every row returned by the query engine includes standard envelope fields:

| Field | Type | Description |
|:------|:-----|:------------|
| `_entity_id` | `u64` | Unique entity identifier |
| `_collection` | `string` | Collection name |
| `_kind` | `string` | Always `"row"` for table rows |
| `_entity_type` | `string` | Entity type classification |
| `_capabilities` | `string[]` | Supported operations |
| `_score` | `f64` | Relevance score (for ranked queries) |

## Column Types

Rows support all 48 data types in the RedDB type system. Fields are dynamically typed on insert but can be constrained with a schema definition. See [Type System Overview](/types/overview.md).

> [!NOTE]
> Collections are created implicitly on first insert. You can also create them explicitly with `CREATE TABLE` or the DDL API.
