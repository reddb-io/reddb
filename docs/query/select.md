# SELECT

The `SELECT` statement retrieves records from a collection with optional filtering, sorting,
grouping, and pagination.

## Syntax

```sql
SELECT [columns | *]
FROM table_name [AS alias]
[WHERE condition]
[GROUP BY column [, ...]]
[HAVING condition]
[ORDER BY column [ASC|DESC] [, ...]]
[LIMIT n]
[OFFSET n]
[WITH EXPAND GRAPH [DEPTH n] | CROSS_REFS | ALL]
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

## GROUP BY / HAVING

Group results by one or more columns. You can combine `GROUP BY` with `HAVING` to filter groups and with `ORDER BY` to sort them.

### Group by a Single Column

```sql
SELECT status FROM users GROUP BY status
```

### Group by Multiple Columns

```sql
SELECT dept, role FROM employees GROUP BY dept, role
```

### Filter and Sort Groups

```sql
SELECT dept FROM employees GROUP BY dept HAVING dept > 5 ORDER BY dept
```

`HAVING` applies its condition **after** grouping, so it filters entire groups rather than individual rows.

### Time-Series Grouping with `time_bucket()`

For native time-series collections, you can group samples into fixed windows directly in SQL:

```sql
SELECT time_bucket(5m) AS bucket,
       avg(value) AS avg_value,
       count(*) AS samples
FROM cpu_metrics
WHERE metric = 'cpu.idle'
GROUP BY time_bucket(5m)
ORDER BY bucket ASC
```

Time-series records expose `metric`, `value`, `timestamp_ns`, and the aliases `timestamp` / `time`,
plus `tags` as a JSON object. `time_bucket(5m)` uses the record timestamp automatically. If you need
an explicit column, use `time_bucket(5m, timestamp_ns)`.

## WITH EXPAND

`WITH EXPAND` triggers automatic discovery of related entities. When present, RedDB performs a secondary lookup after the initial query and includes graph neighbors and cross-referenced entities in the result set.

### Expand via Graph Edges

Perform a BFS traversal from every entity returned by the query. `DEPTH` controls how many hops to follow.

```sql
SELECT * FROM customers WHERE cpf = '000.000.000-00' WITH EXPAND GRAPH DEPTH 2
```

### Expand via Cross-References

Include entities that share cross-referenced identifiers with the matched rows.

```sql
SELECT * FROM ANY WHERE name = 'Alice' WITH EXPAND CROSS_REFS
```

### Expand All (Graph + Cross-Refs)

Combine both expansion strategies in a single pass.

```sql
SELECT * FROM hosts WITH EXPAND ALL
```

### Combine Graph and Cross-Refs Explicitly

```sql
SELECT * FROM users WITH EXPAND GRAPH, CROSS_REFS
```

> [!TIP]
> `WITH EXPAND ALL` is equivalent to `WITH EXPAND GRAPH, CROSS_REFS`. Use the explicit form when you need to set a `DEPTH` for the graph traversal.

## Query Explain

Get the execution plan without running the query:

```bash
grpcurl -plaintext \
  -d '{"query": "SELECT * FROM users WHERE age > 21"}' \
  127.0.0.1:50051 reddb.v1.RedDb/ExplainQuery
```

> [!NOTE]
> Envelope fields prefixed with `_` (like `_entity_id`, `_collection`, `_kind`) are always included in results for entity identification.
