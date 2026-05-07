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

## Performance gate

- **Scenario-specific gate** — per ADR 0009, RedDB does not commit to "20% faster than every competitor on every scenario". Instead, it commits to winning where the unified-engine architecture structurally outperforms (typed_insert, disk_usage, cross-model queries) and to parity-or-close-gap elsewhere.
