# RedDB Domain Glossary

Reusable vocabulary for code, docs, and architecture decisions. New terms join this file as they crystallize during design discussions; this is the canonical place to disambiguate domain language.

## Cache

- **Blob Cache** — the native tiered (L1 RAM + L2 disk) byte-oriented cache module living under `crates/reddb-server/src/storage/cache/`. Operates by `(namespace, key)` lookup. Distinct from the page cache (which caches database pages, not user-visible blobs) and the result cache (which caches SQL result rows; itself now a Blob Cache adapter per #143).
- **L1** — in-memory, sharded SIEVE-evicted hot tier of the Blob Cache. Default 256 MiB, 64 shards.
- **L2** — durable on-disk tier of the Blob Cache. Default 4 GiB. Uses B+ tree metadata + native blob chains. WAL-ordered metadata-last.
- **Synopsis** — per-namespace negative-only Bloom filter for fast L2 misses. Default ~12 KB/namespace at 1% FPR. Returns `MaybePresent` on hit (caller must verify against L2 metadata for an authoritative answer).
- **CachePresence** — three-valued return type from `BlobCache::exists`: `Present`, `Absent`, `MaybePresent`. The synopsis can return `MaybePresent`; only the metadata B+ tree can return `Present` authoritatively.
- **Namespace** — top-level partition in Blob Cache, capped at 256 in MVP. Separate quota, separate generation counter (for O(1) flush), separate synopsis filter.
- **Generation counter** — per-namespace u64 used to invalidate all entries in a namespace in O(1) by bumping the counter; old entries become invisible without walking each key.
- **L1Admission** — policy enum (`Always`, `Auto`, `Never`) deciding whether a put inserts into L1 or skips straight to L2.
- **AsyncPromotionPool** — bounded background task pool that runs L1 promotion when an L2 hit happens, so the read caller doesn't pay the promotion cost in their latency budget.
- **L2BlobCompressor** — content-type-aware zstd wrapper that compresses L2 blobs above a size threshold. Two on-disk variants: `Raw` (tag=0) and `Zstd` (tag=1 + 4-byte original_len).
- **ExtendedTtlPolicy** — opt-in extension to the cache policy carrying `idle_ttl_ms`, `stale_serve_ms`, and `jitter_pct`. Off by default; per-entry rather than global.

## Replication & Topology

- **Topology** — canonical wire payload describing primary + replicas + each peer's region/health/lag/last-applied-LSN. Encoded by a shared encoder consumed by both RedWire HelloAck and the gRPC `Topology` RPC.
- **TopologyAdvertiser** — server-side deep module that turns the live replication state into a `Topology` payload, gated by the `cluster:topology:read` capability.
- **TopologyConsumer** — client-side deep module that parses an advertised payload, merges it against URI seed hints, and emits `ClusterMembership` with refresh hooks.
- **HealthAwareRouter** — client routing layer with EWMA RTT tracking + circuit breaker, replacing dumb modulo round-robin.

## Storage Engine

- **WAL spool** — versioned (v2) write-ahead log records used for replication streaming. Includes magic, version byte, lsn, timestamp, payload-len, payload, crc32. `sync_all` after every append.
- **Logical change applier** — replica-side path that consumes WAL records and applies them, bypassing the public WriteGate.
- **Page cache** — internal sharded SIEVE cache of database pages. Distinct from the Blob Cache. Lives at `storage/engine/page_cache.rs`.

## Query

- **Statement frame** — the per-query lifecycle wrapper (`runtime/statement_frame.rs`) that owns parsing, scope resolution, execution dispatch, result-cache decision, and timing metadata. Single hop every query crosses, regardless of transport.
- **Result cache** — SQL result-row cache with three backends (`Legacy`, `BlobCache`, `Shadow`). Selected via `runtime.result_cache.backend` config knob.
- **AggregateQueryPlanner** — push-down GROUP BY planner that materializes O(group count) instead of O(row count).
- **AskPipeline** — 4-stage funnel for the AI ASK command: token extraction → schema vocabulary match → vector search scoped → value filter. Stage 1 is opt-in heuristic-or-LLM via `ai.ner.backend` config.

## Auth & Security

- **EffectiveScope** — per-request authorization context combining tenant identity + auth principal + capability set. Carried through the statement frame to every authorization check.
- **Capability** — string identifier (e.g., `cluster:topology:read`, `ai:ner:read`) gating an operation. Today the engine has the seam but no real capability checker — placeholder returns `false` for non-trivial capabilities.
- **HeaderEscapeGuard / AuditFieldEscaper / SerializedJsonField / ConnStringSanitizer** — typed guards from PRD #173 enforcing escape-safe construction at HTTP / audit / JSON / connection-string boundaries.
- **Tainted&lt;T&gt;** — wrapper requiring explicit `escape_for(boundary)` re-serialization before a connection-string-derived value crosses any other boundary (log, header, audit, JSON).

## Telemetry

- **Telemetry channel** — one of three logical buckets RedDB emits structured events into: **operator-grade events** (audit log + ops dashboard), **slow query log** (perf-tuning bucket), or **developer signal** (filterable diagnostic noise via `tracing`). Each channel has its own sink, retention policy, and intended audience.
- **OperatorEvent** — closed enum of paging-or-forensic events the operator must notice within minutes (paging) or audit later (forensic). Always-emit, never-sampled, never-filterable. Persisted via the existing AuditLogger (audit log channel) plus a parallel `tracing::warn!(target: "reddb::operator")` breadcrumb for ops dashboards.
- **Slow query log** — separate sink (`red-slow.log`) populated only when query duration exceeds `runtime.log.slow_query_threshold_ms`. Distinct file, distinct rotation policy, distinct retention. Mirror of PG `auto_explain` / `log_min_duration_statement`.
- **AuditLogger** — structured JSON-line emitter with file rotation, secret redaction, and typed fields via `AuditFieldEscaper` (per PRD #177). The sink for operator-grade events.
- **Lossy log writer** — `tracing_appender::NonBlocking` configured with `lossy: true`: hot path pushes onto an MPSC channel; a dedicated worker thread owns the file/stderr fd. On buffer saturation, lines drop rather than backpressure the hot path. Mirror of PG's `logging_collector` subprocess pattern.

## Catalog & Discovery

- **Collection** — root container in RedDB. Every persisted dataset is a Collection regardless of model. The `model` discriminator narrows to `table`, `document`, `queue`, `vector`, `graph`, `timeseries`, or `kv`. Name resolution, ACLs, and storage segments all happen at Collection granularity.
- **CollectionDescriptor** — canonical metadata struct (`catalog.rs:33`) emitted by the catalog snapshot: name, model, schema_mode, entities count, segments count, indices, attention flags. Source of truth for any introspection surface (HTTP `/catalog`, SQL `SHOW`, Postgres-wire `pg_class`).
- **`SHOW COLLECTIONS`** — canonical SQL/RQL discovery command. Lists every Collection regardless of model. **Not yet implemented** as of 2026-05-08; today only `GET /catalog` HTTP exposes the snapshot.
- **`SHOW TABLES` / `SHOW GRAPHS` / `SHOW QUEUES` / `SHOW VECTORS` / `SHOW DOCUMENTS` / `SHOW TIMESERIES` / `SHOW KV`** — typed filters over `SHOW COLLECTIONS`. Each returns only Collections whose `model` matches the keyword. **Not yet implemented**. Faithful-to-type rule: a user asking "show me my tables" should not see queues mixed in.
- **`red` schema** — reserved schema namespace for RedDB-native virtual tables exposing engine introspection: `red.collections`, `red.indices`, `red.stats`, etc. The `red.*` prefix is the canonical RedDB-native form; `pg_catalog.*` views (`pg_class`, `pg_tables`, `pg_indexes`) layer Postgres-wire compatibility on top by translating column shape. Distinct from the column-level `red_*` prefix used for synthetic fields like `red_entity_id` and `red_capabilities`.
- **`SHOW COLLECTIONS` desugaring** — parser-level macro: `SHOW COLLECTIONS` → `SELECT name, model, schema_mode, entities, segments, indices, in_memory_bytes, on_disk_bytes, attention FROM red.collections`. Typed variants apply a `WHERE model = '<kind>'` filter.
- **Wire adapter** — each non-native wire (Postgres, future MySQL/Mongo) is a translation layer in its own `wire/<protocol>/` module. The engine speaks **only** RedDB-native concepts (`red.collections`, `SHOW COLLECTIONS`, etc). Adapters interpret incoming protocol-specific introspection (PG `pg_class`/`pg_attribute`, Mongo `listCollections`) and rewrite to the native form before the query reaches the engine. Postgres-specific concepts like `relkind`, OIDs, and `attnotnull` live exclusively in `wire/postgres/translator.rs`. See ADR 0010.
- **Internal collection** — `CollectionDescriptor.internal: bool` flag (to be added) hiding system-managed collections (DLQs declared via `WITH DLQ`, audit_log, auto-policy artifacts) from default `SHOW COLLECTIONS` output. `SHOW COLLECTIONS INCLUDING INTERNAL` reveals them. Tenant filtering still applies — internal collections are scoped, not invisible.
- **`red.*` read access** — universally readable by any authenticated principal *within their `EffectiveScope`*. No capability check on read; tenant filtering is mandatory and enforced by the engine, not by user-defined policies. Write/update on `red.*` is gated by `cluster:admin`. See ADR 0011 §read access.
- **Catalog snapshot freshness** — `red.*` columns split into two consistency tiers: hot fields (`name`, `model`, `entities`, `segments`, `attention`, `in_memory_bytes`) read directly from the live catalog snapshot every query (sub-ms). Cold fields requiring B-tree walks (`on_disk_bytes`) cache 30s per-collection. Read-after-write within the same node is strong; cross-node in clusters is eventually consistent.

## Keyed Collection Models

- **KV** — keyed Collection model for volatile or high-churn application data. Normal KV is the only keyed model that supports Redis-flavor operations such as TTL/EXPIRE, INCR/DECR, ADD/merge, destructive tag invalidation, and physical DELETE semantics.
- **Config** — keyed Collection model for stable operational configuration. Config values are readable as plaintext, may be typed/schema-validated, keep versioned history for rollback, and never support TTL or counter-style mutation.
- **Vault** — keyed Collection model for sealed secrets. Vault values are encrypted before WAL/page/snapshot persistence, `GET VAULT` returns redacted metadata only, and plaintext reads require explicit `UNSEAL` plus audit.
- **SecretRef** — Config value that points at a Vault secret. `GET CONFIG` returns the reference, not plaintext; resolving it is an explicit operation that also requires Vault unseal permission.
- **Unseal** — privileged plaintext read from Vault. Every unseal attempt is audited and is distinct from metadata reads.
- **Rotate** — versioned replacement operation for Config and Vault. Used for safe rollout/rollback of stable settings and secrets.
- **Purge** — privileged irreversible removal of Config/Vault history. Normal `DELETE` on Config/Vault creates a tombstone version instead.
- **`red.config` / `red.vault`** — bootstrap-created system collections for engine-managed configuration and secrets. Legacy pseudo-paths such as `$config.*`, `$secret.*`, and `red.secret.*` normalize to these explicit system collections; they are not the canonical public model.

## Events & Subscriptions

- **Event-enabled collection** — a Collection (table/document/vector/graph/timeseries/kv) declared with `WITH EVENTS [TO <queue>] [REDACT (...)] [WHERE ...]`. Mutations to it emit events to a queue. Queues themselves cannot emit events (loop prevention).
- **Auto-event queue** — when `WITH EVENTS` omits `TO`, engine auto-creates queue `<collection>_events` with mode `FANOUT`. Visible in `SHOW COLLECTIONS` (not internal).
- **Event payload** — JSON envelope: `op` (insert/update/delete/truncate/drop), `collection`, `id` (PK if declared, else synthetic), `ts`, `lsn`, `tenant`, `before`, `after`. Per-collection ordered by `lsn`. `event_id = BLAKE3(collection || id || lsn || op)` for idempotency.
- **Current event durability gap** — `WITH EVENTS` currently writes the source mutation and the queue message as separate store WAL batches in autocommit mode. This creates a crash window where the row can be durable without its event. See `.red/adr/0015-events-dual-write-window.md`; the intended fix is a true internal outbox or same-batch queue write.
- **REDACT clause** — subscription-level field redaction at producer time. `WITH EVENTS TO audit REDACT (email, phone)` strips those fields from `before`/`after` before enqueueing. Engine warns (not errors) when source has `DENY select` policies on columns not covered by REDACT.
- **Subscription** — `(source_collection, target_queue, operations, filter, redact)` tuple persisted in catalog. Multiple subscriptions per collection allowed. Tenant-scoped by default; cross-tenant requires `events:cluster_subscribe` capability. Creating one needs `select` on source + `write` on target queue.
- **EVENTS BACKFILL** — operator command `EVENTS BACKFILL <collection> [WHERE ...] TO <queue> [LIMIT N]` enqueues synthetic-flagged events for existing rows. Idempotent via deterministic `event_id`. Default subscription has no backfill — only future mutations.
- **Synthetic event** — event with `synthetic: true` produced by BACKFILL. Distinct from real-time events so consumers can choose to ignore historical payloads.

## Queue modes

- **`FANOUT` queue** — every consumer (or consumer group) receives every message. Equivalent to publishing across multiple Kafka consumer groups or RabbitMQ fanout exchange. Default mode for auto-event queues.
- **`WORK` queue** — consumers compete for messages; each message delivered to one consumer in the group. Equivalent to RabbitMQ work queue or Pulsar Shared subscription. Default for `CREATE QUEUE` without explicit mode.
- **Queue delivery** — the lifecycle step that selects an available queue message for a consumer group, records it as pending, and updates attempt counters according to the queue mode.
- **Pending delivery** — a queue message copy that has been delivered to one consumer group but has not yet been ACKed, NACKed, claimed by another consumer, or retired.
- **Queue retirement** — the lifecycle step that ends a pending delivery by acknowledging it for one group, moving it to a DLQ, dropping it, or physically deleting it when queue mode semantics allow.
- **QueueLifecycle** — deep Module owning the full delivery + retirement state machine for both `WORK` and `FANOUT` queues: pick → lock → ack/nack/drop/DLQ. Replaces today's split between `runtime/impl_queue.rs` and `runtime/queue_delivery.rs`. Participates in the caller's transaction (statement frame), reclaims expired locks lazily on next `deliver()`, owns retry/DLQ policy read from queue catalog metadata. Depends on a narrow `QueueStore` adapter trait (not `&Engine`) — primary executes decisions; replicas replay WAL outcomes via the Logical change applier.

## Performance gate

- **Scenario-specific gate** — per ADR 0009, RedDB does not commit to "20% faster than every competitor on every scenario". Instead, it commits to winning where the unified-engine architecture structurally outperforms (typed_insert, disk_usage, cross-model queries) and to parity-or-close-gap elsewhere.
