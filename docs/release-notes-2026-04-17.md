# Release Notes — 2026-04-16 → 2026-04-17

Window: last 24 hours of work on `main` plus uncommitted changes on
the working tree. Grouped by subsystem, ordered roughly by user
visibility.

## Multi-tenancy

### Auto-index on `TENANT BY` column

Declaring `TENANT BY (col)` (or retrofitting via
`ALTER TABLE ... ENABLE TENANCY ON (col)`) now auto-builds a hash
index named `__tenant_idx_{table}` on the discriminator column. Every
read/write against a tenant-scoped table carries an implicit
`col = CURRENT_TENANT()` predicate from the auto-policy, so the index
keeps that predicate on an O(1) seek path instead of forcing a full
collection scan.

```sql
CREATE TABLE orders (id INT, amount DECIMAL, client_id TEXT)
  TENANT BY (client_id);
-- internally: hash index __tenant_idx_orders ON orders (client_id)
```

Skip rules — the auto-index is omitted when:

- the tenant column is dotted (`metadata.tenant`) — flat secondary
  indices don't cover nested paths today
- `__tenant_idx_{table}` is already present (idempotent across boot
  rehydrate)
- a user-defined index already covers the column as its leading key
  (avoids redundant duplicates of a composite index)

`ALTER TABLE ... DISABLE TENANCY` and `DROP TABLE` clean the
auto-index up.

### Dotted tenant paths

`TENANT BY (root.sub.path)` now works on any model that stores
nested data — `metadata.tenant` for vectors, `payload.tenant` for
queue messages, `tags.cluster` for timeseries, `headers.org` for
request-scoped tables. The parser accepts arbitrary depth, and
common JSON field names that happen to be reserved tokens (`meta`,
`data`, `tags`) still parse via `expect_ident_or_keyword`.

Read path: `resolve_runtime_document_path` no longer gates on the
`red_capabilities` document flag — dotted paths resolve against any
column, JSON or Text-that-looks-like-JSON. `Value::Text` payloads
that begin with `{` or `[` are parsed lazily so callers who store
JSON in TEXT columns get the same RLS gating as declared JSON.

Write path: `maybe_inject_tenant_column` routes dotted columns to
`inject_dotted_tenant`, which merges `{ "tenant": "..." }` into the
existing root JSON in place (preserving every other key). When the
root column is absent the helper synthesises a fresh `Value::Json`.
Admin bulk-loaders that already populate the dotted path skip the
merge via `dotted_tail_already_set`.

### Tenant-aware result cache

The query result cache key now mixes in the current tenant id and
auth identity, joined by a unit-separator byte (`\u001E`). Without
this, the sequence

```sql
SET TENANT 'acme';
SELECT * FROM orders;
SET TENANT 'globex';
SELECT * FROM orders;          -- previously: served acme rows from cache
```

served the first tenant's filtered rows back to the second. Fixed.

### Tenant-scoped SEARCH CONTEXT / ASK

The RLS + MVCC gate the SELECT path uses now applies to the context
search pipeline that feeds `ASK`. When a session binds a tenant and
a collection declares `TENANT BY (col)`, the LLM reasons only over
data the caller can see.

`search_entity_allowed(collection, entity, snap_ctx, rls_cache)` is
the per-entity gate, applied across all three search tiers:
field-index hit, token-index hit, global-scan hit. Visibility cuts
first via MVCC, then the RLS filter is resolved per-collection
(cached for the call) and evaluated through
`evaluate_entity_filter_with_db`.

Restrictive default: RLS enabled with zero matching policies ⇒
deny. Mirrors SELECT, so admins never accidentally leak untagged
rows into an AI answer.

## Row-Level Security

### Universal RLS — policies for every entity kind

`CREATE POLICY` now targets non-tabular models directly:

```sql
CREATE POLICY p_node_iso  ON NODES    OF social_graph  USING (...);
CREATE POLICY p_edge_iso  ON EDGES    OF social_graph  USING (...);
CREATE POLICY p_vec_iso   ON VECTORS  OF embeddings    USING (...);
CREATE POLICY p_msg_iso   ON MESSAGES OF jobs_queue    USING (...);
CREATE POLICY p_pt_iso    ON POINTS   OF metrics_ts    USING (...);
CREATE POLICY p_doc_iso   ON DOCUMENTS OF kv           USING (...);
```

A new `PolicyTargetKind` enum (`Table | Nodes | Edges | Vectors |
Messages | Points | Documents`) is stored on `CreatePolicyQuery`.
The evaluator filters policies by kind so a graph policy only gates
graph reads, a vector policy only gates vector reads, etc. The
`ON <table>` short form keeps defaulting to `Table` for full
backwards compatibility.

## MVCC — cross-model atomic transactions

`BEGIN` / `COMMIT` / `ROLLBACK` now span tables, graphs, vectors,
queues, and timeseries atomically — previously every non-tabular
write committed eagerly outside the txn boundary.

- `stamp_xmin_if_in_txn` is the new post-save hook on
  `RedDBRuntime` that stamps `xmin` on entities inserted through
  the DevX builder API. Keeps the storage / runtime layer
  separation clean.
- Hook is applied in `create_node_unchecked` / `create_edge_unchecked`
  / `create_vector` / `create_document` and inline in queue PUSH +
  timeseries point insert.
- Visibility filter wired into:
  - `graph_dsl::materialize_graph_with_projection` (parallel-safe
    via `capture_current_snapshot`)
  - queue `load_queue_message_views`
  - vector ANN post-filter on `db.similar()` results
- `delete_message_with_state` becomes an MVCC tombstone when
  called inside a transaction: `xmax` stamped, `pending_tombstones`
  recorded, physical delete deferred to `COMMIT`, `xmax` revived
  on `ROLLBACK`.
- MVCC helpers promoted to `pub` via the `runtime::mvcc` re-export
  module so transports (wire protocol / gRPC / HTTP) and tests can
  emulate per-connection isolation.
- `detect_mode` recognises one-word transaction / admin commands
  (BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE / VACUUM /
  ANALYZE / RESET) plus `SET TENANT` / `SHOW TENANT`.

Integration test `tests/e2e_cross_model_tx.rs` verifies graph nodes
stay invisible to other connections until COMMIT, and queue ACK
inside a transaction is a tombstone that ROLLBACK revives.

## Storage correctness

A bundle of correctness fixes around the sealed → growing rewrite
path landed inside the 24h window:

- **Race-free sealed delete** — sealed segment mutations no longer
  race with concurrent readers; the rewrite path acquires the
  canonical mutation lock before touching pages.
- **Table-scoped result cache** — invalidation is now scoped to
  the affected collection, not global, removing a noisy
  performance regression on multi-table workloads.
- **Arc-shared prepared statements** — prepared plans share their
  compiled tree via `Arc`, eliminating a per-execute clone hot in
  high-RPS workloads.
- **CDC / context-index gated on confirmed deletion** — emit only
  after the underlying row is actually gone, not on best-effort
  intent. Stops phantom CDC events.
- **Flat-path metadata cleanup** — drop-collection now reaps
  `partition.*` and `tenant_tables.*` markers from `red_config`.
- **Canonical sealed-segment mutations + durable bulk persist** —
  bulk write paths flush to the pager before acknowledging.
- **Sealed-mutation, delete-ordering, unsafe-flag-rename, and
  spill-codec-type fixes** — four critical correctness bugs
  resolved as a single bundle.
- **Metadata persistence in binary file format
  (`STORE_VERSION_V7`)** — previously JSON-on-disk metadata
  migrated into the V7 binary container; faster cold open, fewer
  disk syscalls.
- **Centralised sealed → growing rewrite + persist UPDATE to
  pager** — UPDATE on sealed pages now goes through the same
  rewrite-and-persist code path as INSERT.

## Time-series

- **BRIN-style indexing** — zone maps + block ranges + binary
  search for time-bounded scans. Range queries that previously
  scanned every block now binary-search the zone map and visit
  only the blocks whose `[min_ts, max_ts]` intersects the
  predicate window.
- **Correctness + lock contention fix in BRIN index** — readers
  no longer block writers during zone-map updates; index
  rebuilds happen under a finer-grained lock.

## Query engine — performance

- **Unified mutation engine + slot-indexed aggregates** — one code
  path for INSERT / UPDATE / DELETE writes against every
  collection kind; aggregates index entities by slot for O(1)
  pull instead of linear scan.
- **Covered queries** — projections that are entirely satisfied
  by an index column set skip the entity heap fetch.
- **AND-of-sorted intersection** — multi-predicate filters that
  hit two sorted indices intersect at the index layer instead of
  scanning then filtering.
- **`SplitBlockBloomFilter` for large IN-lists** — `WHERE col IN
  (...big list...)` switches to a split-block bloom filter for
  membership tests; cuts the per-row probe cost dramatically on
  thousand-element IN clauses.
- **Prepared statements share Arc-wrapped plan trees** — see
  storage-correctness section above.

## Roadmap

A 19-category relational SQL parity roadmap was committed
(`81a6155`). The categories cover transactions & MVCC, RLS,
tenancy, views, foreign data wrappers, wire protocol, prepared
statements, savepoints, sequences, expressions, JSON paths,
window functions, full-text, geometry, time-series, replication,
backup / PITR, indexes, and observability. Each item ships
independently — this release closes the first three (MVCC,
RLS, tenancy) end-to-end.

## Tests added in window

- `tests/e2e_tenant_auto_index.rs` — six tests covering CREATE
  TABLE, no-op when no tenancy, ALTER ENABLE retrofit over
  existing data, ALTER DISABLE drop, dotted-path skip, and
  no-duplicate when a user index already covers the column.
- `tests/e2e_tenancy_dotted.rs` — three tests covering dotted
  tenant filter on read, INSERT auto-creating the root JSON
  object, and INSERT merging into an existing root JSON.
- `tests/e2e_ask_tenant_scoped.rs` — ASK corpus respects the
  active tenant: cross-tenant rows are invisible to the LLM.
- `tests/e2e_cross_model_tx.rs` — graph node invisible to other
  connection until COMMIT; queue ACK inside tx is a tombstone
  that ROLLBACK revives.
- `tests/e2e_rls_universal.rs` — queue MESSAGES policies filter
  independently from TABLE policies on the same collection.
