# Cache

RedDB can support cache workloads in two different ways:

- **Key-Value** stores queryable JSON-compatible values as collection records.
- **Blob Cache** is the native cache module for arbitrary bytes, rich
  TTL, fast existence checks, and explicit invalidation.

Use Blob Cache when the value is primarily a cached artifact, not a record you
want to query with SQL.

## When To Use Blob Cache

| Use case | Better fit |
|:---------|:-----------|
| Feature flags, config, small JSON session state | Key-Value |
| Cached HTTP responses, rendered pages, model outputs, binary blobs | Blob Cache |
| SQL `SELECT` result reuse | Runtime result cache with optional Blob Cache backend |
| Embedding-similarity answer reuse | Semantic cache |
| Durable job flow | Queues |

Blob Cache is not meant to replace every Redis data type. It targets exact-key
cache entries where RedDB can add value with SQL-adjacent dependency
invalidation, a bounded payload budget and unified observability.

## Architecture

```text
application / runtime caller
        |
        v
Blob Cache Interface
        |
        +-- L1 memory cache
        |     hot blobs, byte-bounded, sharded
        |
        +-- bounded maintenance worker
              rotating TTL and namespace-generation sweep
```

The normal server runtime is process-local and memory-only. Its dedicated
maintenance worker reclaims expired and namespace-flushed entries in small,
rotating batches without using query workers. Cache state and invalidation are
node-local; the HTTP API does not add replication or cluster-wide coherence.

The engine crate also contains an opt-in file-backed L2 implementation. It is
not enabled by the server because it currently needs a separate pager file,
which is incompatible with RedDB's single-file packaging requirement. A server
restart therefore starts this cache cold.

## TTL Policy

Blob Cache policy is designed to be stricter than a simple "expires after N
seconds" setting.

| Policy field | Meaning |
|:-------------|:--------|
| `ttl_ms` | Hard lifetime after insert. |
| `expires_at_unix_ms` | Absolute expiry timestamp. |
| `idle_ttl_ms` | Sliding expiry after last hit. |
| `stale_ttl_ms` | Optional serve-stale window. |
| `jitter_pct` | Randomizes expiry to avoid thundering herds. |
| `priority` | Biases L1 admission and eviction. |
| `dependencies` | Invalidates entries when related tables / collections change. |
| `tags` | Manual invalidation groups. |

Hard expiry is authoritative. A caller only receives stale data when it asks for
stale serving and the entry is still inside the stale window.

This table describes native engine policy. The remote HTTP contract exposes
hard TTL, absolute expiry, admission, priority and version controls; it does not
expose idle TTL, stale serving, jitter or dependency registration. See
[Remote Blob Cache](/api/cache.md) for the exact application contract.

## Existence Checks

Blob Cache should make "does this key exist?" cheap without trusting
probabilistic data as truth.

1. Check L1 for a hot exact entry.
2. If an embedded caller explicitly enabled L2, check its namespace synopsis.
3. If the synopsis says absent, skip L2.
4. If the synopsis says maybe, verify against L2 metadata for an exact answer.

False positives are acceptable because they only cause an extra metadata read.
False hits are not acceptable.

## Invalidation

Blob Cache needs explicit invalidation APIs because cache correctness usually
depends on domain events, not only TTL.

Expected internal operations:

```text
invalidate_key(namespace, key)
invalidate_prefix(namespace, prefix)
invalidate_tags(namespace, tags)
invalidate_dependencies(dependencies)
flush_namespace(namespace)
sweep_expired(limit)
```

The runtime result cache can map SQL table dependencies into
`invalidate_dependencies`. Product code can use tags for domain groups such as
`tenant:acme`, `user:42`, or `dashboard:revenue`.

## RedDB vs Redis

Redis is the better answer when the workload is purely memory-resident and needs
the mature Redis data-type ecosystem.

RedDB's cache angle is different:

- Hot hits come from L1 memory.
- Invalidation can use table / collection dependencies inside the database.
- Cached blobs live under the same operational umbrella as the rest of RedDB.
- The remote API supports binary values, batches and TTL mutation without a
  second service.

That makes Blob Cache strongest for applications that already use RedDB and want
to avoid running a separate Redis instance for modest, node-local exact-key
cached artifacts. Redis remains the stronger choice for distributed cache
coherence, a mature data-structure ecosystem or consistently sub-millisecond
network tails.

## Current Status

The native module is accessible through HTTP `/cache/ns/*` and `db.cache.*`
using the remote JavaScript client's HTTP transport. It provides exact-key and
batch reads/writes, hard TTL and absolute expiry, `expire`/`persist`, tags,
prefix invalidation and namespace flush. It is a supporting module, not a SQL
`CREATE CACHE` collection. See [Remote Blob Cache](/api/cache.md) for the exact
limits, capacity configuration and persistence boundary.
Other engine cache layers are documented in [SIEVE Cache](/engine/cache.md).
