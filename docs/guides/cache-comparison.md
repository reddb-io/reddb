# Cache vs KV vs Redis: which RedDB surface fits

Status: 2026-11-09 — current server behavior. Companion to
[ADR 0006 — Tiered Blob Cache](../../.red/adr/0006-tiered-blob-cache.md).

RedDB exposes several cache-shaped surfaces. They are not interchangeable
and they exist for different reasons. This guide answers two questions:

1. Inside RedDB, which model should hold *this* piece of data?
2. When do I pick RedDB Blob Cache vs an external Redis?

If you only need the short answer, skim the two tables below and stop.
The rest of this page exists so the answer is defensible six months from
now when the workload has shifted.

---

## Picking between RedDB models

RedDB ships more than one place to put a key. The right one depends on
what the *value* looks like and how you want it invalidated.

| If your data is...                                                          | Use                                                                                                          |
|-----------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------|
| Queryable JSON config / sessions / flags                                    | KV                                                                                                           |
| Opaque byte blobs with TTL and invalidation                                 | Blob Cache                                                                                                   |
| Embedding vectors with similarity lookup                                    | Semantic Cache                                                                                               |
| SQL result rows materialized for re-read                                    | Built-in result cache (now Blob Cache backed when `runtime.result_cache.backend = blob_cache`)               |
| Database pages                                                              | Page Cache (internal, not user-facing)                                                                       |

The four user-visible caches in RedDB today are KV, Blob Cache, Semantic
Cache, and the runtime SQL result cache. Page Cache exists but it
protects database pages on the read path and is never addressed by
application code.

### Why these are separate modules

KV is a *queryable record model* on top of the unified collection layer.
You can `SELECT` it, index it, and replicate it as part of normal
collection state. The cost of that is record-shaped overhead per entry
and an Interface tuned for JSON values, not arbitrary bytes.

Blob Cache is the opposite: a small exact-key Interface (`get`, `put`,
`exists`, `invalidate_*`) over byte payloads with a real cache policy
(hard TTL, absolute expiry, byte capacity, dependency / tag / prefix
invalidation, namespace generation flush). It is not queryable. It does
not appear in SQL. It is the right home for "I just want fast bytes
back" workloads.

Semantic Cache keys by *embedding similarity*, not by exact key. It
exists for prompt / RAG response caching where two equivalent prompts
should hit the same entry even though their byte representation differs.
ADR 0006 leaves the door open for Semantic Cache to use Blob Cache as
its durable exact-key backend, but its lookup surface stays similarity.

The runtime SQL result cache stores small `SELECT` results keyed by
statement shape and scope, invalidated through table dependencies. With
ADR 0006 in flight it becomes an adapter over Blob Cache when
`runtime.result_cache.backend = blob_cache`. The Interface that
application SQL callers see does not change; the storage underneath does.
Tenant and auth identity stay part of the key. Table writes flush the
result-cache namespace when a dependency matches or L2 entries are untracked;
otherwise unrelated results stay cached. The normal runtime cache is
backed by the single-file L2 and can rehydrate eligible entries after restart.

Page Cache and Entity Cache are internal acceleration structures.
They are listed here only so readers stop trying to use them as
application caches — see ADR 0005 for entity cache and ADR 0006 for the
page cache / Blob Cache split.

---

## Picking between RedDB Blob Cache and Redis

RedDB does not try to win by copying Redis' memory model. The useful RedDB
angle is "a bounded exact-key cache with SQL-adjacent invalidation, in the
same process as the data". Redis is still the right answer for a large set of
workloads.

| Need                                                                   | Pick           |
|------------------------------------------------------------------------|----------------|
| Sub-ms tail latency, memory-only                                       | Redis          |
| Survive cache-process restart with warm data                            | Both           |
| Database-aware invalidation tied to writes                             | RedDB          |
| Pub/sub, sorted sets, streams, modules                                 | Redis          |
| Single operational engine for app + cache                              | RedDB          |
| Massive horizontal cluster scale-out, sharded                          | Redis Cluster  |

If you find yourself wanting half of each list, the honest answer is
"run both" — cache invalidation tied to RedDB writes does not stop you
from also running Redis as a memory-only sidecar for pub/sub.

### How to read the table

"Sub-ms tail latency, memory-only" is structural. RedDB Blob Cache has a
fast in-process L1 hit path, but the public remote API uses HTTP and JSON with
base64 for bytes. If you need consistent sub-millisecond
p99 against a memory-resident hot set with no durability requirement,
Redis is already optimized for that and RedDB is not chasing it.

The normal RedDB server cache stores L2 values in a hidden internal collection
inside the primary `.rdb`, so eligible entries survive a clean restart without
an extra cache file. Redis can also preserve warm data with its persistence
options. In either product, treat cache values as regenerable rather than as
the authoritative copy.

"Database-aware invalidation tied to writes" is the killer feature for
read-heavy SQL apps. The same write that mutates a table can invalidate
every cache entry that depended on that table, in the same engine,
without the application having to remember to issue a `DEL` to a
separate cache. See "Invalidation primitives" below.

"Pub/sub, sorted sets, streams, modules" is the Redis ecosystem. RedDB
has separate queues and probabilistic structures, but it does not
implement Redis' data types in this module and is not planning to.

"Single operational engine" is real but boring: one process, one backup
target, one replication topology, one set of metrics, one upgrade path.
For small and mid-size deployments this is often the deciding factor.

"Massive horizontal cluster scale-out" is honest. Redis Cluster shards
across hundreds of nodes today. RedDB Blob Cache is node-local and does not
yet replicate entries or propagate invalidations across the cluster (see
"Out of scope").

---

## L1 / L2 conceptually

**L1** is a process-local, byte-bounded, sharded in-memory tier. It is
sized in bytes, not entries, and shares no allocations with the page
cache — protecting database pages and protecting user-visible cached
bytes are different jobs and the two tiers stay separate so neither
Interface goes shallow. Hits clone a handle, not the payload.

**L2** is a durable cache store. The server stores its rows and byte counter in
the hidden `red.cache.blob_l2` collection in the same `.rdb`; each value change
and quota-counter change share one WAL publication. The record carries expiry,
namespace generation, priority, version and checksum metadata. Tag and
dependency invalidation conservatively clears the affected L2 namespace so an
unrehydrated entry cannot reappear after a write. After restart, eligible
entries hydrate from L2 instead of missing to the origin.

Expiry deadlines also enter the store's persisted ordered projection. A
dedicated cache worker deletes due L2 rows in bounded batches, with the byte
counter in the same WAL publication, and the normal MVCC reclamation path makes
their pages reusable.

Direct engine callers can still select the legacy standalone pager backend for
compatibility, but the normal server does not create that sidecar.

When L2 is enabled, the tiers are connected by a *membership synopsis* — a per-namespace
filter that lets the cache cheaply say "definitely absent" and skip the
L2 metadata read entirely on a miss. See "Existence checks" below.

---

## TTL axes shipped in MVP

The MVP ships two TTL axes:

- **Hard TTL** (`ttl_ms`) — wall-clock time-to-live from insertion.
  Once it fires the entry is gone, regardless of access.
- **Absolute expiry** (`expires_at_unix_ms`) — for callers that compute
  the expiry timestamp themselves (signed-URL-style lifetimes, JWT
  parity, externally-coordinated expiry windows).

Hard expiry always wins. Expired entries are not returned from `get`.

### Engine-only extended policy

The native engine API also supports:

- **Idle TTL** (`idle_ttl_ms`) — sliding expiry for session-like
  entries.
- **Stale TTL** (`stale_ttl_ms`) — optional serve-stale window for
  refresh-ahead workflows. Hard expiry still wins.
- **Jitter** (`jitter_pct`) — randomize expiry to avoid synchronized
  expiry spikes.

These fields are not exposed by the remote HTTP API yet.

---

## Invalidation primitives

Invalidation is part of the Interface, not an afterthought. The MVP
exposes:

- **Key invalidation** — direct overwrite or delete of `(namespace,
  key)`.
- **Prefix invalidation** — drop every key under a prefix inside a
  namespace. Useful for application keys shaped like
  `tenant/acme/user/42/...`.
- **Tag invalidation** — drop every entry tagged with one of the given
  tags. Tags are manual groups like `tenant:acme` or `user:42`.
- **Dependency invalidation** — drop every entry that depends on a
  given table, collection, query, or domain key. This is the seam
  through which SQL writes invalidate cache entries automatically.
- **Namespace flush (generation bump)** — O(1) on the foreground path.
  Bump the namespace generation; old entries become invisible
  immediately and a background sweeper reclaims them.

Dependency invalidation is currently an engine API used by the SQL result
cache adapter. The public HTTP API does not let an application register table
dependencies; remote callers should use tags, prefixes or explicit
invalidation after a database write.

---

## Existence checks and the synopsis-as-negative-filter contract

Blob Cache's `exists(namespace, key)` returns `Present`, `Absent`, or
`MaybePresent`.

- `Absent` is authoritative. The membership synopsis said the key is
  not in the namespace and the cache trusts that answer. No L2 read.
- `Present` is authoritative. L1 or verified L2 metadata confirmed it.
- `MaybePresent` is the "synopsis says maybe, you said you do not want
  to pay for an L2 metadata read" answer. Callers that need an exact
  answer call `get(...)`, which performs the metadata verification.

The contract is: **the synopsis is a negative filter, never the source
of truth for a positive answer**. A Bloom-style filter can have stale
bits after deletes and TTL expiry, and that is fine because it causes
extra verification, never an incorrect hit. The synopsis is rebuilt
from L2 metadata on open (or checkpoint + generation, with rebuild as
the fallback).

If you find yourself relying on `MaybePresent` as if it were `Present`,
that is a bug in the caller, not the cache.

---

## Per-namespace byte limits, replication, backup

**Capacity.** L1 capacity is enforced globally in bytes and is capped by the
process-wide memory budget. Namespace count is separately bounded. A
per-namespace byte quota is not implemented yet, so mutually untrusted noisy
namespaces still need separate instances or application-level quotas.

**Replication contract: node-local only.** Cache state is not replicated.
Each server maintains its own L1. Invalidation is *per-node* — the write
path that issues `invalidate_dependencies(...)` issues it on the node
that processed the write. Cluster-wide invalidation propagation is
explicitly out of scope for the first cut and will be its own ADR
(see "Out of scope").

In practice this means: in a single-writer / read-replica topology,
invalidation works the way you expect because writes go through one
node. In a multi-writer or split-brain-tolerant topology, you should
treat each node's Blob Cache as a separate cache for correctness
purposes until the cluster invalidation ADR lands.

**Backup and restore.** The server L2 is an internal collection in the primary
`.rdb`, so normal single-file snapshots and restores carry it with the database.
The separate-file compatibility backend retains opt-in backup helpers for
direct engine callers.

---

## Reference benchmark numbers

The existing
[`native Blob Cache benchmark`](../perf/blob-cache-bench-2026-05-06.md)
is useful for engine regression work, but its hot-path comparison calls RedDB
in-process while Redis crosses loopback TCP. It is not evidence that the
public RedDB HTTP cache is faster than Redis. A product comparison must put
both systems behind equivalent process boundaries and test the same payload,
batching, concurrency and persistence settings. No remote parity number is
claimed here yet.

The companion perf docs that already exist:

- [`docs/perf/wins.md`](../perf/wins.md) — verifiable wins from the
  canonical `duel-official` methodology lock.
- [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md) — honest
  gaps with closure issues.

Treat any "RedDB Blob Cache vs Redis" number that mixes native RedDB calls
with Redis TCP calls as an internal microbenchmark, not a customer-facing
latency claim.

---

## Out of scope

This is what the Blob Cache module deliberately does **not** do.
Listing them here so nobody has to re-derive the boundary later:

- **Redis protocol parity.** Blob Cache does not speak RESP. If you
  need a wire-compatible Redis, run Redis.
- **Redis pub/sub, sorted sets, streams.** Different data model,
  different module ecosystem, not in scope. RedDB has separate queues
  and probabilistic structures that may grow into adjacent niches, but
  not inside this module.
- **Sub-ms network latency parity.** RedDB Blob Cache's public API uses HTTP.
  If your design point is "everything fits in RAM
  and p99 must be sub-millisecond", Redis already wins that and we are
  not chasing it.
- **Cluster-wide invalidation propagation.** First cut is per-node
  invalidation. Cluster-wide propagation will be its own ADR; do not
  assume it.
- **Public SQL surface.** Blob Cache is intentionally not queryable through
  SQL; use the HTTP/SDK exact-key API.

---

## Public API surface

The Blob Cache exposes itself through **two surfaces, by design**:

1. **Application HTTP/JS SDK** — tenant-scoped `/cache/ns/*` routes for
   exact-key and batch reads/writes, TTL mutation and invalidation.
2. **Operations** — metrics and the existing admin maintenance/statistics
   routes for authorized operators.

**Explicitly killed in this decision:**

- **SQL surface** (`CACHE INSERT / SELECT / DELETE`). Adds entanglement
  with the query engine without a measurable adoption win. Cache is a
  byte-blob primitive; SQL pushes it through a query planner that does
  not help exact-key reads.
- **gRPC.** HTTP admin already covers the operator path; the SDKs
  already cover the developer path. A third wire format adds generated-
  code burden + protobuf schema drift without solving any problem the
  SDK + HTTP combination doesn't already solve.

Decision lives here (not in a standalone ADR) so the reader who asks
"how do I call the cache?" finds the answer next to the comparison
narrative and the migration discussion. Bench evidence at
[`docs/perf/blob-cache-bench-2026-05-06.md`](../perf/blob-cache-bench-2026-05-06.md).

---

## Where to go next

- Architectural rationale and the full Interface sketch:
  [ADR 0006 — Tiered Blob Cache](../../.red/adr/0006-tiered-blob-cache.md).
- L2 storage primitive spike:
  [`docs/perf/blob-cache-l2-spike.md`](../perf/blob-cache-l2-spike.md).
- Performance methodology:
  [`docs/perf/wins.md`](../perf/wins.md) and
  [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md).
