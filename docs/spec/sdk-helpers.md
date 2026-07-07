# SDK Helper Spec (v1.0)

Status: stable v1.0 helper contract. Source of truth for every official RedDB
driver — Rust (`reddb-client`), JavaScript, Python, Go, PHP, Dart — and any
third-party driver that wants to advertise spec-conformant rich helpers.

GitHub source issue: https://github.com/reddb-io/reddb/issues/546
Parent PRD: https://github.com/reddb-io/reddb/issues/449

This document supersedes the v0.1 draft at `docs/clients/sdk-helper-spec.md`.
That file is now a historical pointer to this spec.

## 1. Scope

This spec covers rich helper APIs for the eight data models RedDB exposes:

| Model           | Namespace          | First-class helpers | Notes |
|-----------------|--------------------|---------------------|-------|
| Documents       | `documents`        | yes                 | CRUD + patch + list |
| Key/Value       | `kv`               | yes                 | exact-key, tag-scoped |
| Queues          | `queues`           | yes                 | FIFO push/peek/pop/len/purge |
| Transactions    | `tx`               | yes                 | begin/commit/rollback + callback |
| Vectors         | `vectors`          | v1: SQL-only        | helper API marked **provisional** |
| Graph           | `graph`            | v1: SQL-only        | helper API marked **provisional** |
| Time-series     | `timeseries`       | v1: SQL-only        | helper API marked **provisional** |
| Probabilistic   | `probabilistic.*`  | v1: SQL-only        | helper API marked **provisional** |

The four "provisional" namespaces have a stable **wire** surface (raw SQL via
`query()` / `query_with()`), but no first-class helper methods in v1.0. v1.x
will lift the most common operations into helpers as the SQL surface
stabilises; until then drivers MUST surface those operations via raw query.

## 2. Conventions

### 2.1 Naming

Helpers are **snake_case dot-namespaced**: `documents.insert`, `kv.set`,
`queues.push`, `tx.begin`. Driver languages MAY use idiomatic casing on the
left side of the dot — `documents.insert` in Python, `documents.insert()` in
Rust, `documents.insert(...)` in TypeScript — but the namespace and the verb
MUST match this spec character-for-character so that cross-driver docs and
issue searches resolve.

### 2.2 Public identity

Public item identity is the string field `rid`. Drivers MUST NOT expose
`_entity_id` or `entity_id` except in migration notes.

`rid` is lossless across drivers. JavaScript / TypeScript / JSON-only drivers
that cannot represent large integers safely MUST treat `rid` as `string`.

### 2.3 Collection and key names

Collection names and KV keys are exact strings. Drivers MUST NOT normalise
`:`, `/`, `.`, whitespace, or Unicode. The key `characters:hansel` and the
key `characters%3ahansel` are distinct keys.

### 2.4 Shared result envelopes

| Envelope            | Required fields                                            |
|---------------------|-------------------------------------------------------------|
| `QueryResult`       | `columns`, `rows`, `affected`, optional `stats`             |
| `InsertResult`      | `affected` (always 1), `rid`                                |
| `BulkInsertResult`  | `affected`, `rids` in input order                           |
| `DeleteResult`      | `affected`, `deleted` (bool: `affected > 0`)                |
| `ExistsResult`      | `exists` (bool)                                             |
| `ListResult<T>`     | `items`, `affected`, optional `next_cursor`                 |
| `DocumentItem`      | `rid`, `fields` (column → value map)                        |
| `KvItem`            | `collection`, `key`, `value`                                |

Drivers MAY add language-specific accessors but MUST preserve the wire field
names when the envelope is serialised back to JSON for tooling.

### 2.5 Error taxonomy

Every driver exposes a typed `RedDbError` with at least `code`, `message`,
and optional `details`. Canonical codes:

| Code                  | When raised                                                  |
|-----------------------|--------------------------------------------------------------|
| `INVALID_ARGUMENT`    | Helper input is malformed or unsupported (caller bug)         |
| `INVALID_RESPONSE`    | Server returned a payload the helper cannot decode            |
| `NOT_FOUND`           | Collection, item, key, queue, or structure is absent          |
| `CONFLICT`            | Transaction conflict, unique-constraint violation             |
| `UNAUTHENTICATED`     | Missing or invalid credentials                                |
| `PERMISSION_DENIED`   | Authenticated, but lacks permission                           |
| `UNAVAILABLE`         | Transport or backend is not reachable                         |
| `FEATURE_DISABLED`    | Helper requires a Cargo / runtime feature that's off          |
| `INTERNAL`            | Server returned an unexpected internal failure                |

Validation errors MUST be raised **before** the request is sent when the
helper can prove the input invalid locally. Server-side errors MUST preserve
the server code and message verbatim.

## 3. Generic helpers

These live on the top-level client (not under a namespace).

### 3.1 `query(sql)`

Run a SQL statement. Returns `QueryResult`.

- **Input**: `sql: string`.
- **Output**: `QueryResult` with `columns`, `rows`, `affected`.
- **Errors**: `INVALID_ARGUMENT` for empty SQL; server errors pass through.

Example (Rust):

```rust
let r = db.query("SELECT rid, name FROM users WHERE active = true").await?;
for row in &r.rows { /* ... */ }
```

### 3.2 `query_with(sql, params)`

Parameterised query. `$1`, `$2`, ... in `sql` bind to `params[0]`, ...

- **Input**: `sql: string`, `params: list<Value>`.
- **Output**: `QueryResult`.
- **Errors**: `INVALID_ARGUMENT` if `params` length disagrees with `$N`
  placeholders found by the planner; server errors pass through.

Example:

```rust
db.query_with("SELECT * FROM users WHERE id = $1", (42i64,)).await?;
```

### 3.3 `insert(collection, payload)`

Insert one row-like item.

- **Input**: `collection: string`, `payload: JsonValue` (object).
- **Output**: `InsertResult { affected: 1, rid }`.
- **Errors**: `INVALID_ARGUMENT` if `payload` is not a JSON object;
  `NOT_FOUND` if `collection` does not exist and the engine does not
  auto-create it.

### 3.4 `bulk_insert(collection, payloads)`

Insert many row-like items in one round trip.

- **Input**: `collection: string`, `payloads: list<JsonValue>`.
- **Output**: `BulkInsertResult { affected, rids }` where
  `rids.len() == payloads.len()`. Empty `payloads` is a no-op returning
  `{ affected: 0, rids: [] }`.
- **Errors**: `INVALID_ARGUMENT` for non-object entries.

Drivers MAY implement this as a single native bulk request or as an
equivalent transaction; they MUST NOT silently drop per-row identity.

### 3.5 `delete(collection, rid)`

Delete by `rid`.

- **Input**: `collection: string`, `rid: string`.
- **Output**: integer affected count, or `DeleteResult` in higher-level helpers.

## 4. Documents — `documents.*`

### 4.1 `documents.insert(collection, body)`

- **Input**: `collection: string`, `body: JsonValue` (object).
- **Output**: `DocumentItem { rid, fields }`.
- **Errors**: `INVALID_ARGUMENT` if `body` is not a JSON object.

Example:

```rust
let d = db.documents().insert("events", &JsonValue::object([
    ("event_type", JsonValue::string("login")),
    ("attempts", JsonValue::number(2.0)),
])).await?;
assert!(!d.rid.is_empty());
```

### 4.2 `documents.get(collection, rid)`

- **Input**: `collection: string`, `rid: string`.
- **Output**: `DocumentItem`.
- **Errors**: `NOT_FOUND` if no such `rid`.

### 4.3 `documents.list(collection, options)`

- **Input**: `collection: string`, `options: ListOptions { limit?, filter?, order_by?, cursor? }`.
- **Output**: `ListResult<DocumentItem>`.

`order_by` with ties MUST be tie-broken by `rid` for cross-driver determinism.

### 4.4 `documents.patch(collection, rid, patch)`

- **Input**: `collection: string`, `rid: string`, `patch: JsonValue` (object,
  non-empty).
- **Output**: updated `DocumentItem` (echo) when the engine supports
  `RETURNING *`, else the patch input echoed with the original `rid`.
- **Errors**: `INVALID_ARGUMENT` for non-object or empty patch; `NOT_FOUND`
  if no row matches.

Patch semantics in v1.0 are **top-level merge patch**: each key in `patch`
overwrites the corresponding top-level column on the row. Unrelated columns
MUST NOT be dropped.

Out-of-scope for v1.0 (explicitly):

- JSON Patch (RFC 6902) operations (`add`, `remove`, `move`, `copy`, `test`)
  — **out-of-scope**: not all engines support it uniformly; future RFC.
- Array positional patch (`array[3] = ...`) — **out-of-scope**: depends on
  upcoming array-indexing surface in the SQL planner.
- Deep merge of nested objects — **out-of-scope**: top-level overwrite only
  in v1.0 to keep semantics predictable across the JSON / column-blob split.

### 4.5 `documents.delete(collection, rid)`

- **Input**: `collection: string`, `rid: string`.
- **Output**: `DeleteResult { affected, deleted }`.
- **Errors**: server errors pass through. A delete of a missing `rid` is NOT
  an error: it returns `{ affected: 0, deleted: false }`.

## 5. Key/Value — `kv.*`

### 5.1 `kv.set(collection, key, value)`

- **Input**: `collection: string`, `key: string`, `value: JsonValue`.
- **Output**: `QueryResult` (driver-internal; semantically `affected: 1`).
- **Errors**: `INVALID_ARGUMENT` for empty key.

Example:

```rust
db.kv_collection("cache").set("characters:hansel", JsonValue::string("witch"))
    .await?;
```

### 5.2 `kv.get(collection, key)`

- **Input**: `collection: string`, `key: string`.
- **Output**: `Option<KvItem>` — `None` when key missing, **not**
  `NOT_FOUND`.

### 5.3 `kv.exists(collection, key)`

- **Input**: as `kv.get`.
- **Output**: `ExistsResult { exists }`.

### 5.4 `kv.delete(collection, key)`

- **Input**: as `kv.get`.
- **Output**: `DeleteResult { affected, deleted }`.

### 5.5 `kv.list(collection, options)`

- **Input**: `collection: string`, `options: ListOptions`.
- **Output**: `ListResult` — items have `key` and `value` columns.

Out-of-scope for v1.0:

- TTL helpers (`kv.expire(...)`) — **out-of-scope**: TTL is reachable today
  via `WITH TTL` on the underlying `KV PUT` and via partition-TTL config.
  Standalone helper deferred to v1.1.
- Streaming watch over gRPC — **out-of-scope**: only HTTP transport exposes
  `kv.watch` in v1.0 (long-poll). The helper exists but is feature-gated.

## 6. Queues — `queues.*`

Namespace MUST be `queues` (plural) to match the SQL noun (`CREATE QUEUE`,
`QUEUE PUSH`). v0.1 drafted `queue.*` (singular); v1.0 fixes this. Languages
where a single-handle method name reads more naturally in the singular
(Rust's `db.queue()`) MAY alias the namespace; the spec name remains
`queues.*` for cross-driver docs.

### 6.1 `queues.create(name)`

- **Output**: `QueryResult`; idempotent (uses `IF NOT EXISTS`).

### 6.2 `queues.push(name, payload, options?)`

- **Input**: `name: string`, `payload: JsonValue`.
- **Options**:
  - `priority?: integer` — renders `PRIORITY <n>`.
  - `key?: string` — renders `KEY '<key>'` for FIFO-per-entity grouped delivery.
  - `dedup?: string` — renders `DEDUP '<id>'` for producer idempotency.
  - `delay?: string` — renders `DELAY <duration>` where exposed by the driver.
  - `at?: integer` — renders `AVAILABLE AT <unix_ms>` where exposed by the driver.
- **Output**: `QueryResult`. Drivers SHOULD expose `affected` (always 1).
- **Validation**: helpers MUST reject `key` combined with `delay` or `at` locally
  using `INVALID_ARGUMENT` and the engine message `QUEUE PUSH KEY cannot be
  combined with DELAY / AVAILABLE AT`. Other semantic validation is left to the
  engine.
- **Recipes**:
  - Outbox idempotency: push inside the same transaction as the business write
    with `dedup: "outbox:<event-id>"`.
  - FIFO per entity: use one stable `key` per entity, for example
    `key: "acct_123"` for all account-mutating jobs.

### 6.3 `queues.peek(name, limit?)`

- **Output**: `ListResult` — does NOT decrement length.

### 6.4 `queues.pop(name)`

- **Output**: `ListResult`. Empty queue MUST return an empty `items` list,
  NOT `NOT_FOUND`.

### 6.5 `queues.len(name)`

- **Output**: integer.

### 6.6 `queues.purge(name)`

- **Output**: `DeleteResult`.

Out-of-scope for v1.0:

- Priority queues — **out-of-scope**: server supports `PRIORITY` at queue
  creation; helper sugar deferred. Reach via raw `CREATE QUEUE ... PRIORITY`.
- Consumer groups — **out-of-scope**: same reason. Use `QUEUE POP` with
  group via raw SQL until v1.x.
- Dead-letter routing — **out-of-scope**: deferred to v1.x.

## 7. Transactions — `tx.*`

### 7.1 `tx.begin()`, `tx.commit()`, `tx.rollback()`

Imperative form. Each returns `QueryResult`. The driver client is
session-stateful: a `begin` opens a transaction that the next `commit` or
`rollback` closes. Concurrent calls on the same client during an open
transaction MUST serialise.

### 7.2 `tx.run(callback)` (callback form, optional)

Drivers MAY expose:

```text
tx.run(async (txClient) => { ... })
```

- Successful callback → `commit`.
- Thrown / rejected callback → `rollback`, then re-throw.
- Nested `tx.run` → use savepoints, OR reject with `INVALID_ARGUMENT`. The
  driver README MUST state which.

Out-of-scope for v1.0:

- Isolation-level argument on `tx.begin` — **out-of-scope**: engine default
  is the only fully-tested level today.
- Cross-shard transactions — **out-of-scope**: tracked separately under the
  topology PRD.

## 8. Vectors — `vectors.*` (provisional)

v1.0 has **no first-class vector helpers**. The wire surface is stable via
raw SQL. v1.1 will lift `vectors.search` and `vectors.upsert` into helpers.

Required wire patterns (all reachable via `db.query` / `db.query_with`):

```sql
-- Create a vector-indexed collection
CREATE TABLE embeddings (id TEXT, vec VECTOR(384)) WITH INDEX (vec) USING HNSW;

-- Insert via the generic insert helper
INSERT INTO embeddings (id, vec) VALUES ($1, $2);

-- Top-k similarity search
SEARCH VECTOR vec NEAREST $1 K 10 COLLECTION embeddings;
```

Out-of-scope for v1.0:

- `vectors.search(collection, query_vec, k)` — **out-of-scope**: lifted in v1.1.
- `vectors.cluster(collection, k_or_eps)` — **out-of-scope**: K-Means / DBSCAN
  surface lives at `POST /vectors/cluster` today; helper sugar deferred.

## 9. Graph — `graph.*` (provisional)

v1.0 has **no first-class graph helpers**. The wire surface is stable via
raw SQL / Cypher.

Required wire patterns:

```sql
CREATE TABLE follows (src TEXT, dst TEXT) WITH GRAPH EDGE (src, dst);

-- Cypher (auto-detected by the engine)
MATCH (a:User)-[:FOLLOWS]->(b) WHERE a.id = $1 RETURN b.name;

-- SQL graph expand
SELECT * FROM users WITH EXPAND GRAPH DEPTH 2 WHERE id = $1;
```

Out-of-scope for v1.0:

- `graph.shortest_path(src, dst)` — **out-of-scope**: deferred to v1.1.
- `graph.community(...)` — **out-of-scope**: server SQL surface exists;
  helper deferred.

## 10. Time-series — `timeseries.*` (provisional)

v1.0 has **no first-class time-series helpers**. The wire surface is stable.

Required wire patterns:

```sql
CREATE TIMESERIES cpu_metrics RETENTION 90 d;

INSERT INTO cpu_metrics (ts, host, value) VALUES ($1, $2, $3);

SELECT time_bucket('1m', ts) AS m, avg(value)
FROM cpu_metrics
WHERE ts >= now() - INTERVAL '1h'
GROUP BY m ORDER BY m;
```

Out-of-scope for v1.0:

- `timeseries.write(name, points)` batch helper — **out-of-scope**: use
  `bulk_insert` against the underlying table until v1.1.
- `timeseries.downsample(...)` — **out-of-scope**: continuous-aggregate
  config lives on the table today.

## 11. Probabilistic — `probabilistic.*` (provisional)

v1.0 has **no first-class probabilistic helpers**. The wire surface is
stable via SQL commands.

Required wire patterns:

```sql
-- HyperLogLog
CREATE HLL visitors;
HLL ADD visitors 'user1' 'user2';
HLL COUNT visitors;     -- returns { count }  (drivers MAY alias as `cardinality`)

-- Count-min sketch
CREATE SKETCH clicks WIDTH 2000 DEPTH 7;
SKETCH ADD clicks 'btn_a' 5;
SKETCH COUNT clicks 'btn_a';   -- returns { estimate }

-- Cuckoo filter
CREATE FILTER sessions CAPACITY 500000;
FILTER ADD sessions 'session_abc';
FILTER CHECK sessions 'session_abc';   -- returns { exists }
FILTER DELETE sessions 'session_abc';
```

Out-of-scope for v1.0:

- `probabilistic.hll.add` / `.count` helpers — **out-of-scope**: lifted in v1.1.
- `probabilistic.bloom.*` — **out-of-scope**: server exposes Cuckoo today,
  not Bloom; helper rename + bring-up deferred.
- `probabilistic.cms.*` — **out-of-scope**: same as above.

## 12. Conformance harness

The reference conformance harness ships in the `reddb-client` Rust crate at:

```
crates/reddb-client/tests/conformance.rs
```

It exercises the helper contract against `memory://` (in-process engine,
same wire-shaped helper code path that fires against `grpc://`, `http://`,
and `red://` transports — the helper methods themselves are
transport-agnostic). Other-language drivers MUST port the same case list:

| Case ID                              | Coverage |
|--------------------------------------|----------|
| `generic.query.no_params`            | `db.query` round trip |
| `generic.query_with.params`          | Positional `$N` binding |
| `generic.insert.rid`                 | `db.insert` returns `affected = 1` + lossless `rid` |
| `generic.bulk_insert.rids`           | Empty is a no-op; non-empty preserves input order |
| `generic.delete`                     | Delete-by-rid returns `affected = 1` |
| `documents.crud_nested_patch`        | Insert, get, list, patch (unrelated fields preserved), delete |
| `documents.delete_missing_no_error`  | Deleting a missing rid → `affected = 0`, NOT `NOT_FOUND` |
| `documents.patch_empty_rejects`      | Empty patch rejects with `INVALID_ARGUMENT` |
| `kv.exact_key_round_trip`            | Key `characters:hansel` survives set / get / list |
| `kv.missing_get_returns_none`        | `kv.get` of missing key returns `None`, NOT `NOT_FOUND` |
| `kv.delete_returns_envelope`         | `DeleteResult { deleted: true }` |
| `queues.fifo_peek_pop_len`           | Push two, peek first, pop first, then second; FIFO |
| `queues.empty_pop_returns_empty`     | Empty queue pop → empty items, NOT error |
| `queues.purge_resets_len`            | Push N, purge, length == 0 |
| `tx.commit_persists`                 | begin → insert → commit; row is visible |
| `tx.rollback_discards`               | begin → insert → rollback; row is gone |
| `errors.invalid_argument.empty_sql`  | `db.query("")` rejects with `INVALID_ARGUMENT` |
| `errors.not_found.document_get`      | `documents.get` of missing rid → `NOT_FOUND` |
| `wire.vectors.sql_round_trip`        | Provisional: SQL `CREATE TABLE ... VECTOR(...)` + insert + `SEARCH VECTOR` |
| `wire.graph.sql_round_trip`          | Provisional: SQL `WITH EXPAND GRAPH` reaches engine without parse error |
| `wire.timeseries.sql_round_trip`     | Provisional: `CREATE TIMESERIES` + insert |
| `wire.probabilistic.hll_round_trip`  | Provisional: `CREATE HLL` + `HLL ADD` + `HLL COUNT` |

A case is conformant iff (a) the helper accepts the input shape, (b) the
returned envelope contains every required field, (c) errors map to the spec
codes. Driver implementations port these case IDs verbatim so cross-driver
CI dashboards line up.

## 13. README requirements (per driver)

Every official driver README MUST publish:

- install + connection example;
- the helper availability matrix (rows = case IDs above);
- exact return-envelope summary;
- transaction support statement (imperative-only, callback, or none);
- unsupported / out-of-scope helper list with reasons;
- the conformance command (e.g. `cargo test -p reddb-io-client --test conformance`).

A README linter that diffs driver READMEs against this spec is tracked
separately (PRD #449 follow-up). Until then, implementation issues MUST
update README examples manually.

## 14. Versioning

This spec uses semantic versioning. v1.x will add helpers (the provisional
namespaces) without breaking v1.0 callers. v2.0 would only be cut for a
breaking shape change — for example, switching `rid` to a non-string type,
or changing the `DocumentItem` envelope.

Driver minimum versions:

- Rust `reddb-io-client` ≥ 1.2.0 satisfies v1.0.
- Other-language drivers add a `helper_spec_version = "1.0"` constant on
  their client object and assert against it in CI.

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> The public promises this document makes, and the status of each surface.

| Promise | sql | http | redwire | grpc | driver_helpers |
| --- | --- | --- | --- | --- | --- |
| **PSC-008** — KV helpers expose get/put/delete; get of a missing key returns null, delete reports affected. | ✅ supported | ❌ unsupported | ❌ unsupported | ✅ supported | ✅ supported |
| **PSC-009** — Queue helpers expose create/push/peek/pop/len/purge with FIFO semantics; empty pop is not an error. | ✅ supported | ❌ unsupported | ❌ unsupported | ❌ unsupported | ✅ supported |
| **PSC-010** — Transactions are imperative (begin/commit/rollback) plus a run(callback) form; empty SQL rejects with INVALID_ARGUMENT. | ✅ supported | ❌ unsupported | ❌ unsupported | ✅ supported | ✅ supported |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
