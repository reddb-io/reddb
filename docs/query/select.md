# SELECT

The `SELECT` statement retrieves rows from a table with optional filtering, sorting, and pagination.

## Syntax

```sql
SELECT [columns | *]
FROM table_name [AS alias]
[WHERE condition]
[ORDER BY column [ASC|DESC] [, ...]]
[LIMIT n]
[OFFSET n]
```

## Basic Examples

### Select All Columns

```sql
SELECT * FROM users
```

### Select Specific Columns

```sql
SELECT name, email, age FROM users
```

### With Filters

```sql
SELECT * FROM users WHERE age > 21 AND active = true
```

### Ordered Results

```sql
SELECT * FROM users ORDER BY created_at DESC LIMIT 20
```

### With Pagination

```sql
SELECT * FROM hosts ORDER BY ip ASC LIMIT 50 OFFSET 100
```

## Filter Operators

| Operator | Example | Description |
|:---------|:--------|:------------|
| `=` | `age = 30` | Equality |
| `!=` | `status != 'inactive'` | Inequality |
| `>` | `age > 21` | Greater than |
| `>=` | `score >= 90` | Greater than or equal |
| `<` | `price < 100` | Less than |
| `<=` | `count <= 5` | Less than or equal |
| `AND` | `a > 1 AND b < 10` | Logical AND |
| `OR` | `a = 1 OR b = 2` | Logical OR |
| `NOT` | `NOT active` | Logical NOT |
| `LIKE` | `name LIKE '%alice%'` | Pattern matching |
| `IN` | `status IN ('active', 'pending')` | Set membership |
| `IS NULL` | `email IS NULL` | Null check |
| `IS NOT NULL` | `email IS NOT NULL` | Non-null check |
| `BETWEEN` | `age BETWEEN 18 AND 65` | Range check |

## Column Aliases

```sql
SELECT name AS user_name, age AS user_age FROM users
```

## Table Aliases

```sql
SELECT u.name, u.email FROM users AS u WHERE u.active = true
```

## Response Envelope

Every query returns a standard envelope:

```json
{
  "ok": true,
  "mode": "sql",
  "statement": "SELECT * FROM users WHERE age > 21",
  "engine": "table",
  "columns": ["_entity_id", "_collection", "_kind", "name", "email", "age"],
  "record_count": 3,
  "records": [...]
}
```

| Field | Description |
|:------|:------------|
| `ok` | Whether the query succeeded |
| `mode` | Query mode (`sql`, `gremlin`, `sparql`, `natural`) |
| `statement` | The original query string |
| `engine` | Which engine executed (`table`, `graph`, `vector`, `hybrid`) |
| `columns` | Column names in the result set |
| `record_count` | Number of records returned |
| `records` | Array of result records |

## Executing via HTTP

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT name, email FROM users WHERE age > 21 ORDER BY name LIMIT 10"}'
```

## Executing via gRPC

```bash
grpcurl -plaintext \
  -d '{"query": "SELECT * FROM users WHERE active = true"}' \
  127.0.0.1:50051 reddb.v1.RedDb/Query
```

## Query Explain

Get the execution plan without running the query:

```bash
grpcurl -plaintext \
  -d '{"query": "SELECT * FROM users WHERE age > 21"}' \
  127.0.0.1:50051 reddb.v1.RedDb/ExplainQuery
```

> [!NOTE]
> Envelope fields prefixed with `_` (like `_entity_id`, `_collection`, `_kind`) are always included in results for entity identification.
