# ADR 0019 - Rid and multi-model update surface

**Status:** Accepted
**Date:** 2026-05-15
**Supersedes:** -
**Superseded by:** -
**Related ADRs:** [0011 - red schema stability policy](0011-red-schema-stability-policy.md),
[0014 - MVCC history store and transaction recovery](0014-mvcc-history-store-and-transaction-recovery.md),
[0015 - events dual-write window](0015-events-dual-write-window.md)
**Related issues:** -

## Context

RedDB exposes tables, documents, KV, graphs, vectors, time-series, and queues
through one engine. The existing public vocabulary leaks older generic names
and underscore-prefixed fields into SQL, JSON results, docs, tests, HTTP, MCP,
SDKs, events, and CDC. That makes the product surface feel internal and makes
new multi-model mutation syntax harder to explain.

The new update work needs a clean contract for mutating rows, documents, KV
entries, graph nodes, and graph edges. It also needs a clean identifier name
for batching, `ORDER BY`, graph edge endpoints, events, and `RETURNING`.

This ADR records a deliberate breaking vocabulary change and the first version
of the multi-model `UPDATE` contract.

## Decision

### 1. RedDB ID vocabulary

`rid` means **RedDB ID**. It is the universal identifier for any persisted
RedDB item.

The canonical spellings are:

| Surface | Spelling |
| --- | --- |
| SQL field | `rid` |
| JSON / wire field | `rid` |
| Rust type | `Rid` |
| Human docs | RedDB ID |

`rid` is globally unique inside a database, not scoped to a collection.

The generic multi-model noun is `item`. Public docs and new code should use
`item` when referring to a row, document, KV value, graph node, graph edge,
vector, time-series point, or queue message generically.

New public and code vocabulary must not introduce the old generic identifier
names. Existing internal storage code can migrate gradually, but the public
contract moves to `rid`.

### 2. Public item envelope

Every public item shape exposes these system fields:

| Field | Meaning |
| --- | --- |
| `rid` | RedDB ID |
| `collection` | Collection name |
| `kind` | Item kind |
| `tenant` | Effective tenant, if any |
| `created_at` | Creation timestamp |
| `updated_at` | Last real mutation timestamp |

`created_at` and `updated_at` are `TimestampMs` in UTC on the public SQL and
wire surface.

`created_at` is written on insert and immutable. `updated_at` is written on
insert and updated by any real mutation. A no-op update must not advance
`updated_at`.

These fields appear in `SELECT *` and `RETURNING *`.

These names are reserved top-level fields across rows, documents, KV entries,
graph nodes, and graph edges:

```text
rid
collection
kind
tenant
created_at
updated_at
```

An upgrade or boot against existing data that already uses one of those names
as a user top-level field must fail with an explicit reserved-field conflict.
RedDB must not silently rename user data.

### 3. Item kinds

`kind` uses item kinds, not collection models:

```text
row
document
kv
node
edge
vector
point
message
```

Collection models remain separate:

```text
table
document
kv
graph
vector
timeseries
queue
```

For example, a table collection contains `row` items, and a graph collection
contains `node` and `edge` items.

### 4. Graph reserved fields

Graph nodes and edges reserve additional names.

Node fields:

```text
label
node_type
```

Edge fields:

```text
label
from_rid
to_rid
weight
```

Graph edge endpoints use `from_rid` and `to_rid`, not `from` and `to`.

In the first multi-model update version:

- `rid` is immutable.
- Node and edge `label` are immutable.
- Edge `from_rid` and `to_rid` are immutable.
- Edge `weight` is mutable.
- Node `node_type` is mutable.

### 5. Multi-model UPDATE targets

`UPDATE` targets item kinds:

```sql
UPDATE users ROWS SET score += 1 WHERE id = 1
UPDATE docs DOCUMENTS SET retries += 1 WHERE event_type = 'login'
UPDATE config KV SET value += 1 WHERE key = 'max_retries'
UPDATE social NODES SET visits += 1 WHERE label = 'alice'
UPDATE social EDGES SET weight += 0.5 WHERE label = 'FOLLOWS'
```

Omitting the kind means `ROWS`:

```sql
UPDATE users SET score += 1 WHERE id = 1
```

is equivalent to:

```sql
UPDATE users ROWS SET score += 1 WHERE id = 1
```

The update target must be explicit for `DOCUMENTS`, `KV`, `NODES`, and
`EDGES`.

RedDB validates the declared target against the collection model:

- A table collection accepts `ROWS`.
- A document collection accepts `DOCUMENTS`.
- A KV collection accepts `KV`.
- A graph collection accepts `NODES` and `EDGES`.
- A generic or mixed collection can accept explicit item-kind targets and
  filters to that item kind.

One statement targets one item kind. Cross-kind `UPDATE FROM ANY` is not part
of this contract.

### 6. Compound assignment

Compound assignment is update syntax sugar. It is evaluated to a materialized
post-image before storage, WAL, events, indexing, replication, or recovery see
the write.

The first version supports:

```sql
SET x += expr
SET x -= expr
SET x *= expr
SET x /= expr
SET x %= expr
```

It does not support:

```sql
SET x++
SET x ^= expr
```

The field on the left side must be a top-level field in this version. Nested
paths such as `profile.score`, `body.details.retry_count`, or
`value.limits.max_retries` are future work.

Compound assignment requires an existing non-null numeric left-hand value.
Missing, null, and non-numeric left-hand values are statement errors.

Each assignment expression reads the pre-image of the item. Assignments in the
same `SET` list do not observe earlier assignments from that same statement.

### 7. Math functions

RedDB adds a Postgres-compatible numeric function package for use in
expressions, including `SET`, `WHERE`, `ORDER BY` where supported, and
`RETURNING`.

Canonical functions:

```sql
ABS(x)
ROUND(x)
FLOOR(x)
CEIL(x)
SQRT(x)
POWER(x, y)
EXP(x)
LN(x)
LOG(x)
LOG(base, x)
LOG10(x)
SIN(x)
COS(x)
TAN(x)
ASIN(x)
ACOS(x)
ATAN(x)
ATAN2(y, x)
RADIANS(x)
DEGREES(x)
PI()
```

Aliases:

```sql
POW(x, y)     -- POWER(x, y)
ARCSIN(x)     -- ASIN(x)
ARCCOS(x)     -- ACOS(x)
ARCTAN(x)     -- ATAN(x)
```

Advanced mathematical functions return `Float`. Simple functions preserve
type where the existing evaluator already does so, especially `ABS`.

Division by zero, modulo by zero, integer or decimal overflow, and invalid
function domains are errors. RedDB must not silently write `NULL`, `NaN`, or
infinity for these cases.

### 8. Atomicity and concurrency

A multi-row `UPDATE` statement is atomic. If any candidate item fails during
evaluation, authorization, RLS, lock acquisition, MVCC conflict handling,
indexing, event emission, WAL persistence, or storage write, no item from the
statement is changed.

Concurrent read-modify-write updates must not lose increments. If two
transactions run:

```sql
UPDATE config KV SET value += 1 WHERE key = 'counter'
```

against the same item, RedDB may serialize both writes or reject one with a
conflict. The final committed result must not reflect a lost update.

### 9. WHERE, RETURNING, LIMIT, and ORDER BY

`WHERE` sees the same top-level shape that the target item kind exposes for
querying:

- `ROWS`: row fields plus the public envelope.
- `DOCUMENTS`: document body top-level fields plus the public envelope.
- `KV`: `key`, `value`, metadata already exposed as top-level fields, plus the
  public envelope.
- `NODES`: node fields and properties plus the public envelope.
- `EDGES`: edge fields and properties plus the public envelope.

`RETURNING` is supported for all update targets and returns the post-image.

`LIMIT` is supported for all update targets.

`ORDER BY` is supported in update statements only when paired with `LIMIT`.
The first version of update `ORDER BY` accepts only top-level fields and
`ASC` / `DESC`.

When `ORDER BY` has ties and the query does not explicitly include `rid`,
RedDB adds `rid ASC` as the deterministic tie-breaker.

Example:

```sql
UPDATE docs DOCUMENTS
SET retries += 1
WHERE status = 'pending'
ORDER BY created_at ASC
LIMIT 100
RETURNING rid
```

selects the batch as if it had:

```sql
ORDER BY created_at ASC, rid ASC
```

### 10. Permissions, RLS, events, and indexes

Permissions and RLS use the explicit update target:

```text
UPDATE users ROWS       -> row/table update permission
UPDATE docs DOCUMENTS   -> document update permission
UPDATE config KV        -> KV update permission
UPDATE social NODES     -> graph node update permission
UPDATE social EDGES     -> graph edge update permission
```

Policies run before mutation and use the same item shape as `WHERE`.
`RETURNING` must still honor masking and projection rules.

Indexes affected by changed fields are updated as part of the atomic
statement.

Event subscriptions, CDC, WAL, replication, and recovery observe a normal
materialized update. They do not replay symbolic `+=` operations.

## Consequences

- This is a deliberate breaking public-surface change. The old public
  underscore-prefixed item fields and older generic identifier names are
  removed rather than kept as aliases.
- Existing user data that conflicts with reserved names must be renamed before
  upgrading.
- Existing tests, docs, wire payloads, events, SDKs, MCP tools, HTTP routes,
  gRPC messages, and SQL examples that expose the old vocabulary must be
  updated together.
- ADR 0011 remains the default policy for `red.*`, but this ADR is an explicit
  exception for a major vocabulary cleanup.
- The implementation should land in phases: `Rid` vocabulary and public
  envelope first, graph endpoint rename second, math functions third, then
  multi-model `UPDATE`.

## Considered alternatives

**Keep the existing public identifier fields as aliases.** Rejected. The point
of the change is to remove the internal-looking surface, not add one more name.

**Use `id` for RedDB ID.** Rejected. `id` is already the natural user-domain
identifier in table schemas and examples. Making it a system field would make
`WHERE id = 1` ambiguous and hostile to application schemas.

**Use `entity_id`.** Rejected. The product vocabulary now uses RedDB ID
(`rid`) and item, not entity.

**Allow `UPDATE FROM ANY` across multiple item kinds.** Rejected for the first
version. Cross-kind mutation complicates atomicity, permissions, RLS, shape
validation, indexes, and `RETURNING`. Multiple kind-specific statements can be
grouped in an explicit transaction.

**Allow nested update paths in the first version.** Rejected. Nested mutation
needs a separate contract for missing parents, null parents, arrays, type
mismatches, path-level conflicts, index invalidation, and event payload shape.

**Let division by zero or invalid math return `NULL`.** Rejected. In mutation
statements that can silently corrupt data. Errors abort the atomic statement.
