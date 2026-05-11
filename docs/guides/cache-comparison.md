# Cache vs KV vs Redis: which RedDB surface fits

Status: 2026-05-06 — first cut. Companion to
[ADR 0006 — Tiered Blob Cache](../adr/0006-tiered-blob-cache.md).

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
Eligible Blob-backed entries also survive a clean runtime restart via
the per-database L2 files until their 30-second TTL expires. Tenant and
auth identity stay part of the key, and table writes conservatively
flush the result-cache namespace so stale L2 entries are not served.

Page Cache and Entity Cache are internal acceleration structures.
They are listed here only so readers stop trying to use them as
application caches — see ADR 0005 for entity cache and ADR 0006 for the
page cache / Blob Cache split.

---

## Picking between RedDB Blob Cache and Redis

RedDB does not try to win by copying Redis' memory model. The useful
RedDB angle is "cache with a durable L2 and SQL-adjacent invalidation,
in the same engine as the data". Redis is still the right answer for a
large set of workloads.

| Need                                                                   | Pick           |
|------------------------------------------------------------------------|----------------|
| Sub-ms tail latency, memory-only                                       | Redis          |
| Survive restart without separate Redis                                 | RedDB          |
| Database-aware invalidation tied to writes                             | RedDB          |
| Pub/sub, sorted sets, streams, modules                                 | Redis          |
| Single operational engine for app + cache                              | RedDB          |
| Massive horizontal cluster scale-out, sharded                          | Redis Cluster  |

If you find yourself wanting half of each list, the honest answer is
"run both" — cache invalidation tied to RedDB writes does not stop you
from also running Redis as a memory-only sidecar for pub/sub.

### How to read the table

"Sub-ms tail latency, memory-only" is structural. RedDB Blob Cache has a
fast L1 hit path, but its design point is L1 + durable L2 with a
membership synopsis between them. If you need consistent sub-millisecond
p99 against a memory-resident hot set with no durability requirement,
Redis is already optimized for that and RedDB is not chasing it.

"Survive restart without separate Redis" is the inverse. RedDB Blob
Cache's L2 lives in the same database file as the rest of your data and
participates in the same backup, replication, and recovery story. After
a restart, hot entries reload from L2 into L1 instead of cold-missing to
the origin. You do not need to operate a second system to keep a useful
cache warm across deploys.

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
across hundreds of nodes today. RedDB Blob Cache replicates per-node and
does not yet do cluster-wide invalidation propagation (see "Out of
scope").

---

## L1 / L2 conceptually

**L1** is a process-local, byte-bounded, sharded in-memory tier. It is
sized in bytes, not entries, and shares no allocations with the page
cache — protecting database pages and protecting user-visible cached
bytes are different jobs and the two tiers stay separate so neither
Interface goes shallow. Hits clone a handle, not the payload.

**L2** is a durable native cache store inside the database file. It
holds entry metadata (expiry, tags, dependencies, version, blob pointer)
in a metadata B-tree and stores blob bytes in page-aligned chunks
through the existing native blob direction. WAL ordering is
metadata-last: bytes are written first, then the metadata commit makes
the value visible. After restart, hot entries hydrate from L2 instead
of missing to the origin.

The two tiers are connected by a *membership synopsis* — a per-namespace
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

### Follow-ups (gated on demand)

The ADR defines additional axes that are *not* in the first cut:

- **Idle TTL** (`idle_ttl_ms`) — sliding expiry for session-like
  entries.
- **Stale TTL** (`stale_ttl_ms`) — optional serve-stale window for
  refresh-ahead workflows. Hard expiry still wins.
- **Jitter** (`jitter_pct`) — randomize expiry to avoid synchronized
  expiry spikes.

These land when there is a real caller asking for them, not
speculatively. If you need one, open an issue with the workload that
requires it.

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

Dependency invalidation is what makes "cache with a database underneath"
qualitatively different from "cache next to a database". The same write
path that already calls `invalidate_result_cache_for_table` for the SQL
result cache calls `cache.invalidate_dependencies(...)` for Blob Cache,
and your derived application caches drop in lockstep with the table.

---

## Existence checks and the synopsis-as-negative-filter contract

Blob Cache's `exists(namespace, key)` returns `Present`, `Absent`, or
`MaybePresent`.

- `Absent` is authoritative. The membership synopsis said the key is
  not in the namespace and the cache trusts that answer. No L2 read.
- `Present` is authoritative. L1 or verified L2 metadata confirmed it.
- `MaybePresent` is the "synopsis says maybe, you said you do not want
  to pay for an L2 metadata read" answer. Callers that need an exact
  answer call `get(...)` or request an exact `exists`, which forces the
  metadata verification.

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

**Per-namespace byte limits.** Capacity is enforced in bytes, both
globally and per namespace. A noisy cache namespace cannot starve the
page cache or query execution memory because the byte accounting is
explicit. Operators set the limits in the runtime config matrix
alongside other engine quotas.

**Replication contract: per-node invalidation.** The first cut
replicates cache state per node. Each replica maintains its own L1, its
own synopsis, and its own L2. Invalidation is *per-node* — the write
path that issues `invalidate_dependencies(...)` issues it on the node
that processed the write. Cluster-wide invalidation propagation is
explicitly out of scope for the first cut and will be its own ADR
(see "Out of scope").

In practice this means: in a single-writer / read-replica topology,
invalidation works the way you expect because writes go through one
node. In a multi-writer or split-brain-tolerant topology, you should
treat each node's Blob Cache as a separate cache for correctness
purposes until the cluster invalidation ADR lands.

**Backup and restore: opt-in.** Cache state is excluded from default
backups because it is, by definition, regenerable. If you want cache
state included in a backup target — for instance, to keep an L2 warm
across a cold restore — that is opt-in per namespace in the backup
config. The default is "skip; refill from origin on first miss".

---

## Reference benchmark numbers

Numbers are not embedded in this guide. They live in
[`docs/perf/blob-cache-bench-2026-05-06.md`](../perf/blob-cache-bench-2026-05-06.md)
once Lane #149 lands. When that document publishes, this section will
keep saying "see the cited bench" — update the citation in one place,
not the prose here.

The companion perf docs that already exist:

- [`docs/perf/wins.md`](../perf/wins.md) — verifiable wins from the
  canonical `duel-official` methodology lock.
- [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md) — honest
  gaps with closure issues.

Until the cache-specific bench document publishes, treat any "RedDB
Blob Cache vs Redis" number you see in a slide deck as
not-yet-citeable.

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
- **Sub-ms memory-only latency parity.** RedDB Blob Cache is a
  durable-L2 design. If your design point is "everything fits in RAM
  and p99 must be sub-millisecond", Redis already wins that and we are
  not chasing it.
- **Cluster-wide invalidation propagation.** First cut is per-node
  invalidation. Cluster-wide propagation will be its own ADR; do not
  assume it.
- **Public SQL surface in first cut.** Blob Cache is internal first.
  A public HTTP / SQL surface is deferred to issue #151.

---

## Public API surface — decision (2026-05-07, resolves #200)

The Blob Cache exposes itself through **two surfaces, by design**:

1. **SDK** — `@reddb-io/sdk` (JS/TS, #196), `@reddb-io/client` (thin
   remote, #196), and `drivers/python` (#197). Application code calls
   `cache.get / put / invalidate` and gets typed responses without
   protocol code. Developer-facing surface.
2. **HTTP admin** — `/admin/cache/*` endpoints (compare-and-set #195,
   sweep / flush-namespace / stats #198). Operators reach these through
   `red admin cache` CLI subcommands. Operator-facing surface.

**Explicitly killed in this decision:**

- **SQL surface** (`CACHE INSERT / SELECT / DELETE`). Adds entanglement
  with the query engine without a measurable adoption win. Cache is a
  byte-blob primitive; SQL pushes it through a query planner that
  doesn't help here. Bench `fc1d392a` shows L1 hot p50 = 259 ns; any
  SQL-layer wrapping adds tens of microseconds of dispatch and breaks
  the p50 guarantee.
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
  [ADR 0006 — Tiered Blob Cache](../adr/0006-tiered-blob-cache.md).
- L2 storage primitive spike:
  [`docs/perf/blob-cache-l2-spike.md`](../perf/blob-cache-l2-spike.md).
- Performance methodology:
  [`docs/perf/wins.md`](../perf/wins.md) and
  [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md).
