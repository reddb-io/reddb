# PRD: Redis-flavor KV DSL — PUT / GET / INCR / DECR / CAS / WATCH / EXPIRE as engine-native SQL [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/238

Labels: enhancement

GitHub issue number: #238

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Scope Clarification

This PRD covers **normal KV only**: Redis-flavor keyed application data. It must not implement Config or Vault semantics.

Config and Vault are separate keyed Collection models defined by #314. Normal KV is the only model in this PRD that supports TTL/EXPIRE, INCR/DECR, ADD/merge, and destructive tag invalidation.

## Original GitHub Body

## Problem Statement

RedDB's marketing pitch positions us as a drop-in Redis replacement for application-tier caching and normal KV. Today the engine **does** ship a real KV story — `(key, value)` rows on a `KV` collection queryable with full SQL — but it does NOT expose the Redis-flavor verbs developers reach for:

- No `PUT key = value` / `GET key` / `DELETE key` shorthand. Today they have to write `INSERT INTO sessions (key, value) VALUES (...)` / `SELECT value FROM sessions WHERE key = '...'`.
- No `INCR key BY n` / `DECR` / atomic counters. Application code has to wrap a transaction around `SELECT … FOR UPDATE → UPDATE`, which loses to one-call-Redis on every benchmark.
- No `CAS key = expected SET new` for distributed coordination — locks, leader election, idempotent state transitions all rely on this primitive in Redis-shaped apps.
- No `WATCH key` push-stream. Feature-flag fan-out, live config, presence — everything that Redis pub/sub or `__keyspace@*` patterns address — currently requires polling on RedDB.
- TTL is supported via `INSERT … WITH TTL <ms>`, but the Redis-style inline `EXPIRE 60s` clause on a write is missing.

The marketing implication is direct: as long as Redis-shaped code paths require translation work, the "drop-in Redis replacement" pitch is aspirational, not literal. We've already had to walk back landing copy that asserted these verbs exist; users coming in for Redis migrations bounce when they see SQL-first KV ergonomics.

## Solution

Ship a Redis-flavor KV DSL as engine-native SQL. The verbs become real, parsed statements that compile down to the existing KV collection layer — no separate storage path, no second backup story, no parallel data plane:

```
PUT      sessions.\${id} = '<bytes>'  EXPIRE 3600s
GET      sessions.\${id}
DELETE   sessions.\${id}
INCR     rate_limit.\${ip}  BY 1  EXPIRE 60s   -- returns the new value
DECR     stock.\${sku}      BY 1                -- returns the new value
CAS      lock.deploy        EXPECT 'free' SET 'worker-7' EXPIRE 30s
WATCH    feature.new_dashboard   -- streaming subscribe; pushes (op, key, before, after)
```

The verbs are syntactic sugar over the existing KV collection — but that sugar is what lets a developer with Redis muscle memory drop their code in and have it work. Today the SQL-on-collections surface is correct but unfamiliar; the Redis surface is familiar but unimplemented. The PRD closes that gap.

For multi-namespace deployments, a default `kv_default` collection auto-created at boot accepts the bare-key forms (`PUT name = …`); explicit namespaces use the dot-prefix (`PUT sessions.\${id} = …`) and resolve to the corresponding `CREATE TABLE … KV` collection.

## User Stories

1. As a developer migrating from Redis, I want a `PUT key = value` statement, so that my existing `redis.set(key, value)` calls translate 1:1 without rewriting them as INSERT/UPDATE.
2. As a developer migrating from Redis, I want a `GET key` statement that returns the value, so that my `redis.get(key)` calls translate 1:1.
3. As a developer migrating from Redis, I want `DELETE key`, so that my `redis.del(key)` calls translate.
4. As a developer building a rate limiter, I want `INCR key BY n EXPIRE secs` to return the new counter atomically in one round trip, so that I do not have to wrap a SELECT FOR UPDATE + UPDATE in a transaction.
5. As a developer building a distributed lock, I want `CAS key EXPECT old SET new EXPIRE secs`, so that exactly-one-takes-the-lock semantics resolve in one engine call.
6. As a developer building feature-flag fan-out, I want `WATCH key` to stream change events, so that my services react the moment ops flips a flag instead of polling.
7. As an operator, I want `PUT key = value EXPIRE 60s` to evict via the existing engine TTL sweep, so that there is no separate cleanup cron for short-lived KV state.
8. As an operator, I want every `PUT / DELETE / INCR / CAS` to flow through the same WAL + fsync as table writes, so that the durability story remains "one commit, one fsync, one backup".
9. As an operator, I want `WATCH` events to ride the existing CDC channel that result-cache invalidation already uses, so that I do not maintain a separate notification fabric.
10. As a developer, I want `PUT key = value` on a default collection (no prefix) to just work out-of-the-box, so that my Redis-shaped code does not need a `CREATE TABLE … KV` migration before first use.
11. As a developer, I want `PUT sessions.\${id} = …` to route to a named `sessions KV` collection when one exists, so that namespace per workload (sessions, idempotency, render fragments) stays a one-line declaration.
12. As a security engineer, I want IAM-style policies to gate the new verbs the same way they gate SELECT / INSERT today, so that rolling out the DSL does not introduce a permission gap.
13. As a security engineer, I want this PRD to stay scoped to normal KV only, so that Config and Vault can enforce their separate safety contracts in #314.
14. As a developer, I want `PUT key = value IF NOT EXISTS EXPIRE 7d` for idempotency keys, so that webhook retry handling stays one round trip without an explicit CAS.
15. As a developer, I want `INCR key BY n` to refuse non-numeric existing values with a typed error, so that buggy code surfaces at write time rather than corrupting a counter silently.
16. As a developer, I want `WATCH prefix.*` to subscribe to every change under a prefix in addition to single-key watches, so that I can monitor a namespace without one subscription per key.
17. As a driver author, I want each new verb to round-trip through gRPC, HTTP, and the pgwire-compat protocol with the same payload shape, so that all three drivers stay symmetric.
18. As a driver author, I want a typed Node / Python / Rust client surface (`db.kv.put / get / incr / cas / watch`) so that consumers do not need to hand-write SQL strings.
19. As an operator, I want `red doctor` and the runtime stats endpoint to expose KV op counters (`puts`, `gets`, `incrs`, `cas_success`, `cas_conflict`, `watch_streams`), so that capacity planning and incident triage work out-of-the-box.
20. As an operator, I want benchmarks pinned in CI for `PUT / GET / INCR` p50 and p99, so that regressions surface before release.
21. As a developer, I want the existing `INSERT INTO <kv-collection> (key, value) WITH TTL` / `SELECT value FROM <kv-collection> WHERE key = ?` paths to continue working untouched, so that this PRD adds a sugar surface, never rewrites the underlying collection contract.
22. As a developer, I want CAS on a missing key (`CAS key EXPECT NULL SET v`) to act as "create-if-absent", so that I have a single primitive for both create and conditional update.
23. As a developer, I want `INCR` to accept a `BY n` clause where `n` is negative to act as `DECR`, so that the language stays small.
24. As a developer, I want `WATCH` to stream only committed events (no dirty-read or rolled-back transitions), so that downstream consumers do not have to deduplicate or compensate.
25. As an operator, I want WATCH subscriptions to expire after a configurable idle timeout, so that orphan subscriptions do not accumulate after a client disconnects unexpectedly.
26. As a security engineer, I want every KV verb invocation to land in the audit log alongside the principal and the policy decision that admitted it, so that compliance reviews use the same source of truth as table writes.
27. As a developer, I want `PUT` to optionally TAG entries (`TAGS [...]`), so that bulk invalidation by tag works the way it already does in the Cache primitive — symmetry across primitives.
28. As an operator, I want a schema migration story for adopting the DSL on existing databases (the default `kv_default` collection materialises at first PUT), so that existing deployments do not need a separate migration step before the first call.

## Implementation Decisions

### Modules to build or modify

- **KV DSL parser** (`storage/query/parser/kv_dsl`). New top-level statement variants: `KvPut`, `KvGet`, `KvDelete`, `KvIncr`, `KvDecr`, `KvCas`, `KvWatch`. Each carries an optional `Expire { duration_ms }`, an optional `Tags(Vec<String>)`, and `IfNotExists` for `PUT`. The grammar is dispatched from the same top-level entrypoint as the SQL / queue / time-series surfaces.

- **`KvAtomicOps` runtime — deep module, narrow interface.** Single point of truth for the atomic primitives:
  - `set(coll, key, value, opts) -> Receipt`
  - `get(coll, key) -> Option<TypedValue>`
  - `delete(coll, key) -> bool`
  - `incr(coll, key, by, ttl_ms?) -> i64`
  - `cas(coll, key, expected, new, ttl_ms?) -> CasOutcome`
  - All five run inside the engine's existing page-lock + WAL machinery; concurrency safety is engine-native, not application-side.

- **`KvWatchStream` push-notifications.** Subscribe to a key or `prefix.*`, returns a stream of `(key, op, before, after, lsn)` events. Reuses the engine's existing CDC channel (already used for result-cache invalidation). Per-subscription buffer with backpressure-aware drops on slow consumers (records the drop for observability). Idle-timeout configurable per environment.

- **Default `kv_default` collection.** Auto-created on first PUT without a namespace prefix. `CREATE TABLE … KV` for explicit namespaces stays the recommended path for production; the default is convenience for migrations + small projects.

- **Transport surfaces.**
  - **gRPC.** New methods `KvPut`, `KvGet`, `KvIncr`, `KvCas`, `KvWatch` (server-streaming). Same envelope shape as existing operations.
  - **HTTP.** REST mapping: `PUT /collections/<coll>/kv/<key>`, `GET /collections/<coll>/kv/<key>`, `POST /collections/<coll>/kv/<key>/incr`, `POST /collections/<coll>/kv/<key>/cas`, `GET /collections/<coll>/kv/<key>/watch` (Server-Sent Events). The existing collection-KV HTTP endpoints stay; new endpoints add the atomic / streaming verbs.
  - **MCP.** New tools: `reddb_kv_incr`, `reddb_kv_decr`, `reddb_kv_cas`, `reddb_kv_watch`. Existing `reddb_kv_set` / `reddb_kv_get` stay unchanged.
  - **Postgres-wire.** Surface the new statements via the existing simple-query handler. `PUT key = value` is parsed engine-side; the wire-side just round-trips text.

- **Driver SDKs (Node / Python / Rust).** Add `db.kv.put`, `db.kv.get`, `db.kv.delete`, `db.kv.incr`, `db.kv.decr`, `db.kv.cas`, `db.kv.watch` methods. Each method matches the wire shape so a consumer can read the SQL example on the docs page and translate it 1:1 into their language.

- **Policy + audit hookup.** Each new verb is a `PolicyAction` value. The existing policy resolver gains entries: `kv:put`, `kv:get`, `kv:delete`, `kv:incr`, `kv:cas`, `kv:watch`. Audit log records every decision with the same shape used today for SELECT / INSERT.

### Architectural decisions

- **Sugar over substance, never parallel storage.** The DSL is parser-side syntactic sugar that compiles to existing KV collection operations + the new atomic primitives. There is no separate storage engine, no second WAL, no parallel backup story.

- **Default `kv_default` collection materialises on first use.** Avoids a chicken-and-egg migration step. Operators in production can disable the default by setting `red.config.kv.default_collection = false` and require explicit collections.

- **Config and Vault are intentionally out of scope.** The new verbs touch normal KV collections only. Config and Vault retain separate DDL, APIs, policies, and persistence semantics under #314.

- **TTL inherits the existing semantics.** `EXPIRE 60s` clause translates to the `WITH TTL <ms>` annotation already understood by the storage layer; no new eviction codepath.

- **WATCH rides the existing CDC channel** that drives result-cache invalidation. Avoids a parallel notification fabric. Per-subscription queues with bounded buffers + drop counters keep slow consumers from back-pressuring writers.

- **CAS semantics: typed equality on the existing value.** `CAS key EXPECT 'old' SET 'new'` checks the persisted typed `Value`, not its serialised text. `CAS key EXPECT NULL SET 'v'` is the create-if-absent variant.

- **`INCR` numeric typing.** Refuses to operate on a non-numeric existing value with a typed error. `INCR` on a missing key initialises at the `BY` value (consistent with Redis).

- **`PUT … TAGS [...]`** brings the existing Cache tag-invalidation grammar to user KV collections — symmetric across primitives so consumers learn one mental model.

### API contracts

- All new SQL statements return a structured result envelope with the same shape as existing KV ops (collection name, key, op, value, ttl_ms, lsn). `INCR` / `DECR` returns the new value; `CAS` returns `{ ok: bool, current: Value }` so the caller can retry or proceed.
- `WATCH` is server-streaming. Each event carries the LSN so consumers can resume from a checkpoint after reconnect.
- HTTP / gRPC / MCP all transport the same envelope shape.

## Testing Decisions

A good test for this surface verifies the externally observable contract — does `INCR` return `prev + by`, does `CAS` succeed only when `expected` matches, does `WATCH` deliver every committed event in order, does `PUT … EXPIRE …` actually evict at the right time. It does not assert on storage-layer details or specific page IDs.

Modules with priority test coverage:

- **`KvAtomicOps` runtime.** Property-based concurrency tests: 100 goroutines hammering `INCR` on the same key must converge to the right total. CAS race tests: only one of N concurrent CAS calls on the same key with the same `expected` succeeds. TTL: insert with short TTL, sleep past it, assert eviction. Models the existing `runtime` integration tests.

- **`KvDsl` parser.** Snapshot tests against the existing `tests/queue_parser.rs` and `tests/timeseries_parser.rs` style — proptest grammar generators feeding `parser::parse`, FIXME pins for any limitation discovered during dev.

- **`KvWatchStream`.** Stream delivery tests: PUT a key while a WATCH is active, assert the subscriber receives the event with the right `before` / `after`. Slow-consumer tests: ensure backpressure drops are recorded but writers do not block. Disconnect / reconnect tests: the LSN-resume path resumes from the right checkpoint.

- **Transport symmetry.** Same scenario (PUT → GET → INCR → DELETE) executed over each transport (gRPC, HTTP, MCP, pgwire) with the same input. Output envelopes must match.

- **Policy enforcement.** Integration test: principal with `kv:get` only must be able to GET but not PUT or INCR. Audit log assertions on each decision.

- **Benchmarks pinned in CI.** `PUT` p50 / p99 vs Redis baseline on the same workload. `INCR` throughput. `WATCH` event-delivery lag. Regressions block merge.

## Out of Scope

- **Redis pub/sub channels.** `WATCH` covers keyspace notifications — the most common use case — but Redis `SUBSCRIBE` / `PUBLISH` on arbitrary channel names is a different surface and lands as a follow-up.
- **Sorted sets, hashes, lists, streams (XADD).** These are Redis-specific data structures with their own access patterns. RedDB has tables / documents / queues / time-series for the same workloads; we do not need a parallel set of Redis-shaped structures.
- **Lua scripting (EVAL / EVALSHA).** Out of scope for this PRD. Stored procedures are a separate effort with a different security surface.
- **Redis Cluster sharding hash slots.** RedDB's shard story is replication-based, not consistent-hash-based. We do not implement Redis-cluster-compat sharding.
- **MULTI / EXEC transactions.** The engine already exposes ACID transactions through `BEGIN / COMMIT`. The Redis MULTI envelope adds nothing on top.
- **Migration path for application code that already relies on the SQL-on-collections surface.** Existing INSERT / SELECT paths are untouched; this PRD adds verbs alongside them.

## Further Notes

- The `PUT / GET / INCR / CAS / WATCH` shape is the developer-facing competitive lever. Without it the engine is functionally a Redis replacement; with it we win on muscle memory.
- This PRD pairs naturally with **Cache** (TTL eviction + tag invalidation) and with the separate Config/Vault PRD #314, but it must not blur those domains. Normal KV is volatile/high-churn data; Config is stable settings; Vault is sealed secrets.
- Marketing landing copy referencing these verbs is currently aspirational. Once this PRD ships, the landing pages at `/db/key-value` and `/db/cache` get a documentation refresh; until then, those pages call out the SQL-on-collections shape as the canonical way to use KV today.
- A follow-up PRD will cover Redis-compat pub/sub channels (`SUBSCRIBE` / `PUBLISH`) once the WATCH primitive ships and we have user feedback on the shape.
