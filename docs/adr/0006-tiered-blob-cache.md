# ADR 0006: Tiered blob cache as a native cache module

Status: Proposed (2026-05-06)

## Context

RedDB already has several cache-shaped modules, but none of them is the right
Interface for a general-purpose application cache that can compete with Redis
for blob workloads.

- The user-facing KV model stores JSON-compatible `key` / `value` records on top
  of the unified collection layer. It is queryable and useful for config,
  sessions, and flags, but it is not optimized for arbitrary bytes, rich TTL
  policy, or cache admission / eviction.
- The runtime result cache stores small `SELECT` results keyed by statement
  shape and scope. It is intentionally conservative and is invalidated through
  table dependencies.
- The page cache and entity cache are internal acceleration structures. They are
  important, but their Interfaces are tied to pages and `get_any(id)`, not to an
  application cache namespace.
- The semantic cache is closer to the target shape because it has TTL and an
  optional persistent backend, but its lookup Interface is embedding similarity,
  not exact key lookup.

Redis is memory-first. RedDB should not try to win by copying Redis' exact
memory model. The useful RedDB angle is a cache module that combines a fast
process-local L1 with a durable L2 in the database file, while keeping misses
cheap through a membership synopsis.

## Decision

Introduce a native **Blob Cache** module with a small exact-key Interface and a
tiered implementation:

```text
cache_get(namespace, key) / cache_exists(namespace, key)
        |
        v
L1 memory shard
  - exact hot entry map
  - byte-bounded SIEVE or TinyLFU-style admission
  - per-entry expiry metadata
        |
        v
Membership synopsis
  - split-block Bloom or Cuckoo-style filter per namespace
  - negative answer skips L2
  - positive answer verifies against metadata
        |
        v
L2 native cache store
  - namespace + key -> entry header
  - blob bytes stored outside normal row JSON
  - expiry index for sweeps
  - dependency / tag indexes for invalidation
```

The Blob Cache is a **supporting engine structure** first, not a new persisted
entity kind in the unified collection layer. A public HTTP / SQL surface can be
added later, but the first seam should be internal so the runtime result cache,
future table acceleration, and selected product APIs can share one cache engine
without forcing the user-facing KV model to carry cache-specific invariants.

## Interface

The internal Interface should be deliberately small:

```rust
cache.put(namespace, key, blob, policy) -> CacheWriteReceipt
cache.get(namespace, key) -> Option<CacheHit>
cache.exists(namespace, key) -> CachePresence
cache.invalidate_key(namespace, key) -> InvalidationCount
cache.invalidate_prefix(namespace, prefix) -> InvalidationCount
cache.invalidate_tags(namespace, tags) -> InvalidationCount
cache.invalidate_dependencies(dependencies) -> InvalidationCount
cache.sweep_expired(limit) -> SweepReport
cache.stats(namespace) -> CacheStats
```

`CachePresence` should distinguish `Present`, `Absent`, and `MaybePresent`.
`MaybePresent` is useful when the membership synopsis says "possibly present"
but the caller asked for an existence-only fast path and does not want to pay an
L2 metadata read. Callers that need an exact answer call `get` or request an
exact `exists`.

## Cache policy

The policy needs to be richer than the current result cache TTL:

| Field | Purpose |
|:------|:--------|
| `ttl_ms` | Hard time-to-live from insertion. |
| `expires_at_unix_ms` | Absolute expiry for externally computed cache lifetimes. |
| `idle_ttl_ms` | Sliding expiry for session-like entries. |
| `stale_ttl_ms` | Optional serve-stale window for refresh workflows. |
| `jitter_pct` | Avoid synchronized expiry spikes. |
| `priority` | Bias memory admission / eviction. |
| `max_blob_bytes` | Per-namespace guardrail. |
| `l1_admission` | `Always`, `Auto`, or `Never` for large / cold blobs. |
| `dependencies` | Table, collection, query, tenant, or domain dependency keys. |
| `tags` | Manual invalidation groups. |
| `version` | Compare-and-set / generation guard for overwrite races. |

Hard expiry always wins over stale serving. Expired entries must not be returned
from `get` unless the caller explicitly asks for stale data and the entry is
within `stale_ttl_ms`.

## L1 memory

L1 should be byte-bounded, sharded, and independent from the page cache. The page
cache protects database pages; the Blob Cache protects user-visible cached bytes.
Sharing the same module would make both Interfaces shallow.

Recommended shape:

- 64 shards by hash of `(namespace, key)` to reduce lock contention.
- Entry values stored as `Arc<[u8]>` so hits clone handles, not payloads.
- Per-shard SIEVE initially, because RedDB already documents and uses SIEVE for
  page-cache workloads. Consider W-TinyLFU later if admission quality shows up
  in profiles.
- Byte capacity enforced globally and per namespace.
- No write lock on cold invalidation: probe metadata / shard directory first,
  matching the lesson from ADR-0005.

## L2 database store

L2 should not store blobs as JSON `Value::Json` inside normal table rows. That
would inherit row overhead, SQL visibility rules, and collection-model coupling
that a cache does not need.

Recommended shape:

- A native cache catalog keyed by namespace.
- A metadata B-tree keyed by `(namespace_hash, key_hash, key_bytes)` with:
  expiry timestamps, byte length, checksum, tags/dependencies offsets, version,
  content type, encoding, and blob pointer.
- Blob bytes stored in page-aligned chunks using the existing native blob
  direction (`NATIVE_BLOB_MAGIC`) rather than adding another row shape.
- A time index keyed by `(expires_at_unix_ms, namespace_hash, key_hash)` for
  bounded sweeps.
- Dependency and tag indexes keyed by dependency/tag -> cache keys for explicit
  invalidation.

WAL / persistence ordering must be metadata-last: write blob chunks first, then
commit the metadata entry that makes the value visible. Invalidation should
write a tombstone / generation bump before removing old bytes so readers never
resurrect a stale entry from L2 after L1 eviction.

## Membership synopsis

The fast existence check should use a synopsis as a negative filter, not as the
source of truth.

- If the namespace filter says "absent", RedDB can skip the L2 metadata B-tree.
- If the filter says "maybe", RedDB verifies against metadata for `get` and exact
  `exists`.
- Deletes and TTL expiry may leave stale bits in a Bloom-style filter. That is
  acceptable because it causes extra verification, not incorrect hits.
- Rebuild the filter from L2 metadata on open, or checkpoint it with a generation
  and fall back to rebuild when the generation is stale.

This gives the desired "quickly say whether it exists" property without making
cache correctness depend on a probabilistic structure.

## Invalidation

Invalidation is a first-class part of the Interface, not an afterthought.

Internal callers need these paths:

- Key invalidation for direct overwrite / delete.
- Prefix invalidation for namespaced application keys.
- Tag invalidation for manual groups such as `tenant:acme` or `user:42`.
- Dependency invalidation for table / collection / query dependencies. Runtime
  write paths can call this from the same places that currently call
  `invalidate_result_cache_for_table`.
- Generation invalidation for cheap whole-namespace flushes. Bump the namespace
  generation and let old entries become invisible without walking every key
  synchronously.

The generation path is important for product ergonomics: `cache.flush(namespace)`
should be O(1) on the foreground path, with physical cleanup left to a sweeper.

## Relation to existing modules

- **KV** remains the queryable key-value model. Blob Cache is for byte-oriented
  cache semantics, not general records.
- **Runtime result cache** can become an adapter over Blob Cache after the native
  module exists. Its current dependency tracking maps cleanly onto cache
  dependencies.
- **Entity cache** should stay specialized. It caches cloned `UnifiedEntity`
  values for cross-collection lookup and has a narrower, deeper Interface.
- **Semantic cache** can later use Blob Cache as its durable exact-key backend
  while preserving embedding-based lookup in memory.
- **Tables and indexes** can consume the same invalidation and membership
  machinery for derived artifacts, but table rows should not route through Blob
  Cache on the normal read path.

## Redis positioning

RedDB should position this as "cache with durable L2 and SQL-adjacent
invalidation", not "Redis replacement for every Redis data type".

Likely wins:

- Cache entries survive process restart without an external Redis instance.
- Application, SQL result, and derived-table caches use one invalidation model.
- Large blobs can live mostly in L2 while hot entries stay in L1.
- Operators get cache state, indexes, backup, and replication in the same engine.

Likely non-goals:

- Redis pub/sub parity.
- Redis module ecosystem parity.
- Sub-millisecond tail latency for purely memory-resident, network-local
  workloads where Redis is already optimal.
- Redis data types such as sorted sets and streams in this module. RedDB already
  has separate queues and probabilistic structures.

## Rollout

1. Land `storage::cache::blob` as an internal in-memory module with rich TTL,
   sharding, byte capacity, stats, and invalidation tests.
2. Add L2 metadata + blob persistence behind the same Interface.
3. Add membership synopsis and startup rebuild.
4. Move the runtime result cache onto Blob Cache as the first production caller.
5. Add admin / observability endpoints for stats, sweep, and namespace flush.
6. Consider a user-facing HTTP / embedded API once the internal Interface has
   survived real result-cache traffic.

## Validation requirements

Before accepting the implementation, require:

- Unit tests for hard TTL, absolute expiry, idle TTL, stale serving, jitter
  bounds, namespace generation invalidation, key / prefix / tag / dependency
  invalidation, and byte-capacity eviction.
- Persistence tests proving expired entries do not rehydrate and metadata-last
  writes do not expose partial blobs.
- Concurrency tests for hot gets while invalidation and sweeps run.
- Benchmarks versus current runtime result cache and a local Redis baseline for:
  hot L1 hit, cold miss, L2 hit, large blob L2 hit, namespace flush, and table
  dependency invalidation.

## Consequences

**Benefits.**

- Creates a deep cache Interface that can serve multiple RedDB modules.
- Gives RedDB a credible Redis-adjacent story without pretending to be
  memory-only.
- Avoids overloading KV with cache-only policy and invalidation complexity.
- Reuses existing storage strengths: durable pages, WAL, backup, and
  collection-aware invalidation.

**Costs.**

- Adds a new native supporting structure that must participate in recovery,
  backup, metrics, and resource limits.
- Requires careful WAL ordering and tombstone / generation design.
- Needs explicit byte accounting so cache blobs cannot starve the page cache or
  query execution memory.

**Open questions.**

- Whether L1 should use SIEVE first for consistency, or W-TinyLFU immediately
  for better admission under mixed blob sizes.
- Whether the first public surface should be embedded Rust only or HTTP as well.
- Whether namespace-level quotas should live in the existing config matrix or a
  cache-specific catalog.
