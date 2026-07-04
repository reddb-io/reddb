# UPDATE

The `UPDATE` statement modifies RedDB items in a collection. The catalog
infers the model (rows, documents, KV) on existing collections. Only graph
node and edge updates require explicit markers to disambiguate within a
mixed-model graph collection.

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
UPDATE collection_name [NODES|EDGES]
SET field1 = value1 [, field2 += value2, ...]
[WHERE condition]
[ORDER BY field [ASC|DESC] LIMIT n]
[RETURNING * | field [, field ...]]
```

Target meanings:

| Target | Item kind | Notes |
|:-------|:----------|:------|
| omitted | inferred from catalog | Row, document, or KV — the catalog knows the model on existing collections |
| `NODES` | `node` | Updates mutable graph node properties; used in mixed-model graph collections to disambiguate |
| `EDGES` | `edge` | Updates mutable graph edge properties; used in mixed-model graph collections to disambiguate; `from_rid` and `to_rid` are immutable |

## Examples

### Update Rows

```sql
UPDATE users SET age = $1 WHERE name = $2
```

```sql
UPDATE users
SET active = true
WHERE rid = $1
RETURNING rid, name, active
```

### Update Documents and KV

```sql
UPDATE events
SET reviewed = true, attempts += 1
WHERE event_type = 'login'
RETURNING rid, kind, reviewed, attempts
```

```sql
UPDATE config
SET value += 1
WHERE key = 'feature.rollout_percent'
RETURNING rid, key, value
```

### Update Graph Items

```sql
UPDATE social NODES
SET score += 5
WHERE node_type = 'person'
RETURNING rid, label, score
```

```sql
UPDATE social EDGES
SET weight += 0.25
WHERE from_rid = $1
RETURNING rid, from_rid, to_rid, weight
```

> [!WARNING]
> Without a `WHERE` clause, all items in the selected target are updated.

### Compound Assignment

Compound assignment is equivalent to assigning from the pre-image field value.
All right-hand sides in the same statement read the item state before the
update starts.

```sql
UPDATE accounts
SET balance += 25, retries %= 3
WHERE rid = $1
RETURNING rid, balance, retries
```

Supported numeric operators are `+=`, `-=`, `*=`, `/=`, and `%=`.

### Math Functions

Math functions can appear anywhere ordinary SQL expressions are accepted,
including `SET` and `RETURNING` projections:

```sql
UPDATE metrics
SET root_score = SQRT(score), score = POWER(score, 2)
WHERE score >= 0
RETURNING rid, root_score, score
```

PostgreSQL-compatible math functions include `SQRT`, `POWER`/`POW`, `EXP`,
`LN`, `LOG`, `LOG10`, `SIN`, `COS`, `TAN`, `ASIN`/`ARCSIN`,
`ACOS`/`ARCCOS`, `ATAN`/`ARCTAN`, `ATAN2`, `COT`, `DEGREES`, `RADIANS`,
and `PI`.

### Ordered Update Batches

`ORDER BY ... LIMIT` selects a deterministic batch before applying updates.
`ORDER BY` without `LIMIT` is rejected. Ties are broken by implicit `rid ASC`
when `rid` is not already part of the ordering.

```sql
UPDATE jobs
SET claimed = true
WHERE claimed = false
ORDER BY priority DESC
LIMIT 25
RETURNING rid, priority, claimed
```

For all models, ordered batches accept top-level fields only (no nested paths
or computed `ORDER BY` expressions).

### Atomic Failure Behavior

Each `UPDATE` statement validates the selected item batch before committing
the mutation. If any matched item would fail a compound assignment, immutable
field check, RLS/column-policy check, arithmetic check, or type check, the
statement fails and none of the selected items are written.

Immutable public identity/topology fields cannot be mutated. `rid`, graph node
`label`, and graph edge `from_rid` / `to_rid` are rejected in `SET`.

## Via HTTP

### PATCH (by RedDB ID)

```bash
curl -X PATCH http://127.0.0.1:5000/collections/users/entities/102 \
  -H 'content-type: application/json' \
  -d '{"fields": {"age": 31, "active": true}}'
```

### SQL UPDATE via Query

```bash
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query": "UPDATE users SET age = $1 WHERE rid = $2 RETURNING rid, age", "params": [31, 102]}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{
    "collection": "users",
    "id": 102,
    "payloadJson": "{\"fields\":{\"age\":31}}"
  }' \
  127.0.0.1:55055 reddb.v1.RedDb/PatchEntity
```

`UpdateEntityRequest.id` is the retained protobuf field name. Treat its value
as the public RedDB ID `rid`.

## Via MCP

```json
{
  "tool": "reddb_update",
  "arguments": {
    "collection": "users",
    "set": {"age": 31, "active": true},
    "where_filter": "rid = 102"
  }
}
```

## WITH Clauses

You can attach or replace expiration and metadata on existing entities using `WITH` clauses. These are the structured alternative to legacy `_ttl` or `_ttl_ms` column approaches.

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
