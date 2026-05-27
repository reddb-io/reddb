# Red UI Backend Feature Audit

Status: draft
Date: 2026-05-27
Scope: compare the `red-ui` pages (`query`, `collections`, `cluster`,
`security`) against the RedDB surfaces visible in this repo.

## Assumptions

- `red-ui` should use RedDB-native surfaces first: `POST /query`, `red.*`
  virtual tables, `/metrics`, `/stats`, `/replication/status`, and the auth/admin
  HTTP endpoints.
- A collection is the root data container. The model discriminator is the thing
  that specializes the UI into table, document, kv, graph, vector, queue,
  timeseries, metrics, etc.
- Deprecated granular `/catalog/*` endpoints should not be the main UI contract
  when a `red.*` relation exists.

## Learnings From `../red-ui`

- `red-ui` already has a typed protocol client for several proposed surfaces.
  It calls `GET /collections`, optionally probes `GET /collections/:name`,
  posts queries to `/query`, probes `/cluster/status`, and expects auth reads
  from `/auth/whoami`, `/auth/users`, `/auth/tenants`, and `/auth/policies`.
- `GET /collections` is already implemented here and returns
  `{ "collections": [...] }`, matching the current UI client.
- Bare `GET /collections/:name` is not the same contract today. RedDB exposes
  `GET /collections/:name/schema`, while `red-ui` expects metadata directly at
  `GET /collections/:name` with `kind`, `capability`, `capabilities`, `schema`,
  `indexes`, `retention`, `tenant`, and per-action authorization.
- `POST /query` already mostly matches the UI's expected high-level shape:
  `ok`, `query`, `mode`, `capability`, `engine`, `record_count`, and
  `result.columns`/`result.records`. The missing part is richer descriptor
  metadata for renderer selection, column types, cursors, and model-specific
  result envelopes.
- `red-ui` currently infers vectors, queues, stats, and graphs by column names.
  These heuristics are useful as fallback, but they are the wrong product
  contract for an admin UI. The database should expose explicit descriptors for
  vector columns/scalars, queue message state and consumers, stats units/types,
  and graph node/edge envelopes.
- The graph result pane currently chunks `WHERE rid IN (...)` lookups because
  the UI code documents a RedDB behavior where larger `rid IN` lists can return
  zero rows. Either fix that query path or provide a graph viewport/subgraph
  endpoint so the UI does not need this workaround.
- The security page confirms `/auth/users` exists and is useful, but tenants and
  policies are still not exposed under the `/auth/*` names the UI client expects.
  RedDB has admin policy endpoints under `/admin/policies`; the product contract
  should either add `/auth/policies` aliases or teach `red-ui` to use the admin
  surface with the right permissions.

## Already Covered Enough To Build Against

### Query page

- General query execution: `POST /query` accepts SQL/RQL and returns records with
  columns and the public item envelope (`rid`, `collection`, `kind`, `tenant`,
  `created_at`, `updated_at`).
- Query language coverage includes `SELECT`, filters, joins, CTEs, aggregates,
  `INSERT/UPDATE/DELETE ... RETURNING`, `EXPLAIN`, graph commands, vector search,
  KV commands, queue commands, timeseries queries, and Prometheus-style metrics
  queries.
- Multimodel search exists through `SEARCH`, `ASK`, vector/text/hybrid search,
  graph traversal/path commands, and `FROM ANY`.
- Prometheus-compatible metrics query endpoints exist: `/api/v1/query`,
  `/api/v1/query_range`, and `/api/v1/write`.

### Collections page

- Collection discovery exists through `red.collections` and `SHOW COLLECTIONS`.
  Columns include `name`, `model`, `schema_mode`, entity/segment/index counts,
  memory/disk estimates, `internal`, `tenant_id`, queue mode, vector dimension,
  vector metric, and timeseries session fields.
- HTTP collection listing also exists through `GET /collections`, returning the
  `{ collections: string[] }` shape currently used by `red-ui`.
- Schema/index/stats discovery exists through `red.columns`, `red.show_indexes`,
  `red.indices`, and `red.stats`.
- Queue-specific discovery exists through `red.queues` and `SHOW QUEUES`, with
  `name`, `mode`, `depth`, `total_pending`, `oldest_pending_age`, `dlq_target`,
  `attention`, and `internal`.
- Pending queue deliveries have a drill-down relation, `red.queue_pending`, with
  queue/group/message/delivery/attempt/lock/consumer fields.
- Events/subscriptions are visible through `red.subscriptions`.
- Materialized view and retention surfaces exist in runtime code as
  `red.materialized_views` and `red.retention`.

### Model toolbars

- Tables: CRUD via SQL and HTTP, filters, ordering, pagination by
  `LIMIT/OFFSET`, updates, deletes, `RETURNING`, indexes, `SHOW SCHEMA`,
  `SHOW STATS`, views, materialized views, and partial `EXPLAIN SELECT`.
- Documents: first-class document create/bulk insert, JSON path querying,
  document SQL analytics, patch/update coverage, and inferred fields through
  `red.columns` where available.
- KV: HTTP key read/write/delete, SQL `KV GET/PUT/DELETE/INCR/DECR/CAS`, normal
  SQL over `key` and `value`, prefix-like listing through `WHERE key LIKE ...`,
  and JSON-compatible values.
- Vectors: vector insert/bulk insert, auto-embed, similarity search, text search,
  hybrid search, IVF search, vector clustering, dimension/metric metadata, and
  `vector.turbo` search path.
- Queues: create/alter mode, push/pop/peek/read, groups, ack/nack, pending,
  claim, DLQ, move/replay, purge/truncate, per-queue introspection, and queue
  lifecycle metrics.
- Graphs: node/edge insert, node/edge update, `MATCH` partial, neighborhood,
  traverse, shortest path, centrality, community, components, cycles,
  clustering, topological sort, properties, and path queries.
- Timeseries/hypertables: `CREATE TIMESERIES`, `CREATE HYPERTABLE`, retention,
  chunk helpers, `time_bucket()`, session metadata, continuous aggregate helper
  functions, and normal SQL group-by for chart data.
- Metrics/statistics: `CREATE METRICS`, Prometheus remote write, instant/range
  query APIs, counters/gauges, rollups, retention, and tenant/namespace
  isolation.

### Cluster page

- Server mode and embedded mode are documented separately.
- Health/readiness endpoints exist: `/health`, `/ready`, `/ready/query`,
  `/ready/write`, `/ready/repair`, and lifecycle variants.
- `/stats` documents active/idle connections, store counts, total memory, pid,
  CPU cores, total/available memory, OS, arch, and hostname.
- `/deployment/profiles` describes embedded/server/serverless endpoint contracts.
- `/metrics` exposes role, health, DB size, backup/WAL archive, replication lag,
  commit wait, read-only state, quota rejection, and other operational metrics.
- `/replication/status` exists and exposes role/LSN/apply-state style status for
  replication operations.

### Security page

- Users, API keys, groups, legacy roles, IAM policies, RLS, tenants via
  `SET TENANT`/`TENANT BY`, and control evidence are documented and tested.
- `red.users` and `red.api_keys` expose minimized metadata without secrets.
- `red.policies`, `SHOW POLICIES`, `SHOW POLICIES FOR USER`, policy attachment
  APIs, effective-permission APIs, and policy simulation exist.
- Tenant-scoped catalog reads are enforced for `red.*` surfaces.

## Requests For The RedDB Team

### P0 - Needed For A Reliable First Red UI

1. Add a collection metadata endpoint aligned with the current `red-ui` client.

   Needed shape: `GET /collections/:name` or another explicitly documented
   endpoint returning collection `name`, model `kind`, primary `capability`,
   all `capabilities`, schema/system columns, indexes, retention, tenant scope,
   and current-principal actions. Actions should include booleans or
   `{ allowed, reason }` for `select`, `insert`, `update`, `delete`, and
   model-specific operations such as `run_algorithm`, `enqueue`, `dequeue`,
   `ack`, `nack`, and `search_vector`.

   Current gap: `GET /collections/:name/schema` returns entity count, TTL, and
   optional physical contract metadata. It does not give enough information to
   specialize empty collections, drive toolbars, or hide denied actions without
   probing data with `SELECT * LIMIT 1`.

2. Add a single UI-oriented cluster snapshot.

   Needed shape: `GET /cluster/status` or `SELECT * FROM red.cluster_status`
   returning deployment mode (`embedded`, `file`, `server`, `serverless`),
   process role (`standalone`, `primary`, `replica`), data path, listener
   addresses, whether the process appears containerized, host/container ids when
   safe, replicas, per-peer lag, connection counts, database bytes, WAL bytes,
   available storage bytes, CPU utilization, RAM utilization, request rate,
   read rate, write rate, average latency, p95/p99 latency, and last error.

   Current gap: the information is split across `/stats`, `/metrics`,
   `/deployment/profiles`, `/replication/status`, and physical endpoints. Some
   requested values are explicitly missing from the metrics spec:
   `reddb_ops_total`, query duration histograms, `reddb_active_connections`,
   response bytes, and restore progress.

3. Add a machine-readable query result descriptor.

   Needed shape: every `/query` response should include stable metadata such as
   `result_kind`, `models_present`, column names/types/nullability when known,
   item counts by `kind`, whether graph nodes/edges/paths are present, whether
   vector scores are present, and suggested renderer hints (`table`, `json`,
   `graph`, `timeseries`, `metric`, `queue`, `kv`).

   Current gap: the frontend can infer from columns and envelopes, but inference
   will be fragile across multimodel results.

4. Add first-class tenant lifecycle/catalog support, or explicitly bless an
   application-owned tenants table as the product contract.

   Needed shape for `security`: list/create/update/disable/delete tenants, show
   tenant metadata, and show which users/policies/collections are scoped to a
   tenant.

   Current gap: docs state there is no `CREATE TENANT` command and no internal
   tenant registry. Tenants are opaque strings carried by session/current scope.

5. Add security read endpoints that match the admin UI contract.

   Needed shape: `GET /auth/tenants` returning visible tenants,
   `GET /auth/policies` returning policies visible to the current principal, and
   `POST /auth/can` or equivalent batch authorization checks with denial
   reasons. Keep `/auth/users` as the user listing contract; it already returns
   `{ ok, users }`.

   Current gap: `/auth/users` and tenant-scoped user aliases exist, while policy
   listing lives under `/admin/policies` and there is no tenant catalog endpoint.

6. Close security enforcement gaps that matter for an admin UI.

   Needed: policy hooks for SQL DDL, queue-specific IAM resources, direct HTTP
   collection endpoints, graph analytics, vector-specific resource verbs, and
   admin/metrics paths where the UI must make per-user decisions.

   Current gap: docs/security/permissions.md marks several of these as
   role-gated, simulator/audit vocabulary, or not consistently policy-aware.

7. Add queue active-consumer presence.

   Needed shape: per queue/group online consumers, last_seen, current leases,
   pending count, dead consumer/expired lease count, and consumer heartbeat age.

   Current gap: `red.queues` has depth/pending/DLQ summary and
   `red.queue_pending` shows locked deliveries, but there is no online consumer
   registry or active consumer count.

8. Add vector artifact/TurboQuant introspection for the vector toolbar.

   Needed shape: `red.vector_artifacts` or `red.vectors` with collection,
   dimension, metric, index type, build state, encoded artifact status,
   TurboQuant/TurboVec parameters, scalar fallback status, SIMD dispatch, row
   count, bytes, rebuild progress, and last error.

   Current gap: `red.collections` exposes only dimension/metric. Physical
   native-vector artifact endpoints and internal TurboQuant modules are not a
   stable collection-level UI contract.

9. Add faithful typed `red.*` relations for non-queue models.

   Needed: `red.tables`, `red.documents`, `red.kv`, `red.vectors`,
   `red.graphs`, `red.timeseries`, `red.metrics`, each with model-shaped
   columns rather than only `red.collections` rows.

   Current gap: queues already have `red.queues`; most other typed `SHOW`
   commands are still thin filters over `red.collections`.

10. Add hypertable/timeseries UI metadata relations.

   Needed: `red.hypertables`, `red.hypertable_chunks`, and/or
   `red.timeseries_stats` exposing time column, chunk interval, TTL, retention,
   chunk ranges, row counts per chunk, oldest/newest timestamp, downsample tiers,
   continuous aggregates, and sweep status.

   Current gap: SQL helpers exist, but the UI needs stable tabular metadata for
   chart controls and maintenance panels.

### P1 - Important For A Polished Red UI

1. Add a graph viewport/subgraph endpoint or SQL relation.

   Needed shape: given collection, center node(s), depth, filters, and limit,
   return normalized `nodes` and `edges` arrays with labels/properties/weights
   and truncation metadata. Algorithms stay in the database; layout can remain
   frontend-owned.

   Current gap from `red-ui`: the graph pane currently performs multiple SQL
   queries and chunks `rid IN (...)` lists to avoid a documented small-list
   failure. That workaround should not become the durable integration contract.

2. Add saved visualization metadata for metrics/statistics.

   Needed shape: optional DB-backed definitions for gauges, KPIs, charts,
   default PromQL/SQL expressions, thresholds, units, and owner/tenant scope.
   The UI can still render ad hoc charts without this, but saved dashboards
   need a durable place.

3. Add collection-level write/activity rates.

   Needed shape: per collection reads/sec, writes/sec, request/sec, average
   latency, error count, last write timestamp, and last error. This should feed
   both collection list badges and per-collection toolbar status.

   Current gap: `red.stats.last_write_ms` is documented as `NULL` because the
   backing stats APIs do not expose collection-level write timestamps.

4. Add server-side query pagination/cursor support for large UI tables.

   Needed shape: stable cursor token or page token over a snapshot, total/approx
   count when cheap, and cancellation for long queries. `LIMIT/OFFSET` is enough
   for small collections but gets weak for large admin browsing.

5. Add JSON value helpers for KV/document editing.

   Needed shape: validate JSON patch, set/delete nested path, and return parse
   errors with JSON pointer locations. KV can already store JSON-compatible
   values, but the UI editor needs safe partial-update primitives.

### P2 - Nice To Have

1. Add UI-safe sample profiles per collection type.

   Needed shape: `SHOW SAMPLE <collection> WITH PROFILE` or
   `red.collection_samples` with representative rows, field cardinality,
   inferred value types, min/max timestamps, and recommended default renderer.

2. Add explain metadata tuned for UI display.

   Needed shape: `EXPLAIN` JSON plan with stable step ids, estimated/actual
   rows, timings, index usage, and warning flags.

3. Add feature/capability discovery.

   Needed shape: `red.capabilities` or `/capabilities` listing enabled models,
   build flags, auth mode, AI providers, vector SIMD support, replication mode,
   and which preview features are active.

## Do Not Re-request

- Basic collection listing: covered by `red.collections` and `SHOW COLLECTIONS`.
  `GET /collections` also exists for the current `red-ui` protocol client.
- Basic table CRUD/filter/update: covered through SQL and HTTP entity endpoints.
- Basic graph algorithms: shortest path, traversal, centrality, community,
  components, cycles, clustering, topological sort, and properties are present,
  with some parser caveats.
- Basic queue/DLQ operations: covered by queue commands, `red.queues`,
  `red.queue_pending`, and queue telemetry.
- Basic KV-as-JSON storage: KV values are JSON-compatible and queryable as
  `key`/`value`.
- Basic metrics dashboards: Prometheus remote write/query/range query and
  metrics collections exist.
- Basic users/policies: auth users, groups, policy attachment, simulation,
  `red.users`, `red.api_keys`, and `red.policies` exist.

## Evidence Checked

- `docs/reference/red-schema.md`
- `docs/reference/sql-1-0-x.md`
- `docs/api/http.md`
- `docs/data-models/queues.md`
- `docs/data-models/vectors.md`
- `docs/data-models/graphs.md`
- `docs/data-models/key-value.md`
- `docs/security/multi-tenancy.md`
- `docs/security/permissions.md`
- `docs/security/rbac.md`
- `docs/spec/metrics.md`
- `docs/reference/metrics.md`
- `docs/deployment/server.md`
- `docs/deployment/embedded.md`
- `docs/deployment/replication.md`
- `tests/e2e_red_collections_acceptance.rs`
- `tests/e2e_issue_535_red_queues_virtual_table.rs`
- `tests/e2e_red_schema.rs`
- `tests/e2e_metrics_collection_contract.rs`
- `crates/reddb-server/src/server/routing.rs`
- `crates/reddb-server/src/server/handlers_ops.rs`
- `crates/reddb-server/src/server/handlers_auth.rs`
- `crates/reddb-server/src/presentation/query_result_json.rs`
- `crates/reddb-server/src/runtime/red_schema.rs`
- `../red-ui/FEATURES.md`
- `../red-ui/CLAUDE.md`
- `../red-ui/packages/protocol/src/client.ts`
- `../red-ui/packages/ui/src/lib/capability.ts`
- `../red-ui/packages/ui/src/lib/ResultsPane.svelte`
- `../red-ui/packages/ui/src/lib/SchemaTree.svelte`
- `../red-ui/packages/ui/src/routes/cluster/+page.svelte`
- `../red-ui/packages/ui/src/routes/security/+page.svelte`
- `../red-ui/packages/ui/src/lib/renderers/vector-render.ts`
- `../red-ui/packages/ui/src/lib/renderers/queue-render.ts`
- `../red-ui/packages/ui/src/lib/renderers/stats-render.ts`
- `../red-ui/packages/ui/src/lib/renderers/graph-render.ts`
