# Migrating from Redis to RedDB Blob Cache

Status: 2026-05-06 — first cut. Companion to
[ADR 0006 — Tiered Blob Cache](../adr/0006-tiered-blob-cache.md) and
[`docs/guides/cache-comparison.md`](./cache-comparison.md).

This guide is for teams that already run Redis as an application cache
and are evaluating RedDB Blob Cache as the replacement. It assumes you
have already decided Blob Cache is the right home for *this* cache —
[`cache-comparison.md`](./cache-comparison.md) is the document that
helps you decide, this one is for getting there without breaking
production.

The migration is shaped as a three-phase rollout — dual-write, cutover,
decommission — with explicit gaps called out where Redis semantics do
not have a Blob Cache equivalent. Read the "Out of scope" section
first if you depend on pub/sub, sorted sets, streams, Lua, or cluster
hash slots; those are not migration targets.

---

## Read-pattern mapping

The Blob Cache Interface is intentionally small (see ADR 0006 §
"Interface"). Most Redis cache patterns map onto a handful of
operations; a few do not map at all. The table below covers the read
patterns we see most often in cache-shaped Redis workloads.

| Redis op                  | RedDB Blob Cache equivalent                                                                      |
|---------------------------|--------------------------------------------------------------------------------------------------|
| `GET key`                 | `BlobCache::get(namespace, key)`                                                                 |
| `SET key value`           | `BlobCache::put(namespace, key, value, BlobCachePolicy::default())`                              |
| `SET key value EX 60`     | `BlobCache::put(... ttl_ms: Some(60_000) ...)`                                                   |
| `SETEX key 60 value`      | same                                                                                             |
| `DEL key`                 | `BlobCache::invalidate_key(namespace, key)`                                                      |
| `EXPIRE key 60`           | not directly; re-put with new TTL                                                                |
| `EXISTS key`              | `BlobCache::exists(namespace, key)`                                                              |
| `TTL key`                 | not exposed; introspect via stats                                                                |
| `INCR key`                | NOT supported (Blob Cache is opaque bytes; use KV for counters)                                  |
| `KEYS pattern`            | `invalidate_prefix` for bulk ops; no enumeration                                                 |
| `MGET key1 key2`          | sequential `get` calls; no batch primitive yet                                                   |
| `EVAL Lua script`         | NOT supported                                                                                    |
| `MULTI/EXEC`              | NOT supported (no transactions on cache)                                                         |
| `SUBSCRIBE channel`       | NOT supported (no pub/sub)                                                                       |

A few notes on rows that look one-to-one but are not:

- **`SET ... EX 60` vs `BlobCachePolicy { ttl_ms: 60_000 }`.** Both
  are hard wall-clock TTLs. The difference is what happens on read
  — see "TTL semantics" below.
- **`DEL` vs `invalidate_key`.** Returns the count of affected
  entries, not a boolean. Multi-tier (L1 + L2) is handled inside the
  call; callers do not coordinate.
- **`EXISTS` vs `exists`.** Blob Cache can return a third state,
  `MaybePresent`, when the membership synopsis says "possibly
  present" and the caller asked for the cheap path. If you strictly
  need Redis-shaped boolean output, treat `MaybePresent` as `true`
  and pay verification on the next `get`. See ADR 0006 § "Membership
  synopsis" and the cache comparison guide § "Existence checks".
- **`KEYS pattern`.** RedDB Blob Cache deliberately does not support
  enumerate-by-pattern. The supported bulk primitive is
  `invalidate_prefix`, which drops every key under a prefix in a
  namespace without first materializing the keyspace. If you were
  using `KEYS` to *invalidate*, this is the migration target. If you
  were using `KEYS` to *enumerate*, that is a different design problem
  — Blob Cache will not help; consider KV with a real index instead.
- **`MGET`.** No batch primitive yet; sequential `get` is the
  workaround. Most application caches see this as an L1-resident
  pattern where the per-call cost is dominated by lock acquisition,
  not network. If you have a real multi-key fan-out workload that
  needs amortized batching, open an issue with the read pattern.

---

## TTL semantics differences

Redis TTLs come in two flavors that interact with reads:

- **Hard expiry** — `EXPIRE`, `EX`, `PEX`, `EXPIREAT`. Deletes the key
  at the configured time regardless of access.
- **Idle-style** by way of `OBJECT IDLETIME` and `maxmemory-policy
  allkeys-lru / volatile-lru`. Hot keys stay resident even past a
  rough idle bound because LRU keeps refreshing them under load.

RedDB Blob Cache MVP ships **hard TTL** (`ttl_ms`) and **absolute
expiry** (`expires_at_unix_ms`). Both are wall-clock; neither is
extended by reads. If you stored a Redis entry with `SET key value EX
600` and your access pattern kept it warm for hours, that is *not* the
behavior you get from `BlobCachePolicy::default().ttl_ms(600_000)` —
the entry will fall out at the 600-second mark even if you read it
every second.

Idle TTL (`idle_ttl_ms`, sliding expiry) is in the ADR (§ "Cache
policy") as a follow-up axis, gated on real demand. If you depend on
sliding expiry semantics for sessions or hot-key keep-alive, do one of
the following until the follow-up lands:

1. **Re-put on read.** When your read path returns a cache hit, issue
   a `put` with the same value and a fresh `ttl_ms`. This is exactly
   what Redis is doing internally for LRU touch, but explicit. The
   cost is one extra L1 write per hit on a hot key.
2. **Use a longer hard TTL with stale-while-revalidate logic in the
   caller.** This is appropriate when "occasional staleness up to
   `T`" is more tolerable than "occasional cold miss".
3. **Park the workload on Redis until idle TTL ships.** Open an issue
   describing the workload so it gets prioritised.

Hard expiry always wins. Expired entries are not returned from `get`,
matching the cache-comparison guide § "TTL axes shipped in MVP".

---

## Pub/sub absence

RedDB does not implement Redis pub/sub and is not planning to inside
the Blob Cache module. ADR 0006 lists pub/sub explicitly as a
non-goal. The cache-comparison guide makes the same call.

The common reasons applications use Redis pub/sub *for caching* are
cache-bust fan-out, coalescing live update streams, and unrelated
coordination signals. Migration workarounds:

- **Cache-bust fan-out** — use dependency invalidation or namespace
  flush on the writer side. The same engine that processes the write
  also processes the invalidation, so no separate fan-out channel is
  needed for writes that go through RedDB. See ADR 0006 §
  "Invalidation" and the cache comparison guide § "Invalidation
  primitives". Note the per-node contract from ADR 0008: in a
  single-writer / read-replica topology this works as expected; in
  multi-writer treat each node as a separate cache surface.
- **Live update streams** — not a cache problem. RedDB has separate
  queue primitives; do not bend Blob Cache around streams.
- **Unrelated coordination signals** — keep using a coordination
  primitive outside the cache; running both Redis-as-broker and
  RedDB Blob Cache is a reasonable end state.

Where pub/sub was used purely as a cache-bust signal and dependency
invalidation does not cover it (e.g., an external system writes to
RedDB out-of-band), the workaround is **poll-based generation
checks**: the writer bumps a namespace generation through
`invalidate_namespace`; pollers compare the cached generation against
the current one and drop their L1 view when it moves. Strictly worse
than pub/sub on latency, but correct and it does not invent a new
module.

---

## Pipelining

Redis pipelining batches commands across a single connection so the
client pays one round-trip cost for many ops. RedDB Blob Cache does
not have an explicit pipeline primitive yet.

In practice the operations that motivated Redis pipelines fall into
two buckets when ported to Blob Cache:

- **Bulk invalidations.** Already O(1) on the foreground path through
  `invalidate_prefix` or `invalidate_namespace`. One call replaces a
  pipeline of `DEL` per key.
- **Bulk reads / writes.** No batch primitive yet. The L1 hit path
  is process-local when embedded — no network round-trip to amortize
  — so the win from a hypothetical batch primitive is smaller than
  against Redis. For network-fronted use a batch primitive may land
  later; track the issue when it opens.

If your existing Redis pipelining is doing several `DEL` after a
domain write, that is exactly the `invalidate_dependencies` workload.
Move the dependency declaration to the `put` site and the dependency
invalidation to the writer; the per-key `DEL` storm goes away.

---

## Cluster-mode

Redis Cluster shards the keyspace by hash slot across many nodes,
with client-side or proxy-side routing. RedDB does not have a
hash-slot model. ADR 0008 describes the topology as **single-primary
plus read replicas**, with cache state replicated **per-node**.

Concretely:

- **Sharding.** There is no Blob Cache hash-slot equivalent. If your
  Redis deployment depends on cluster-mode for raw keyspace size or
  per-shard memory, RedDB Blob Cache MVP does not match that scale
  point. The cache comparison guide flags this honestly under
  "Massive horizontal cluster scale-out".
- **Invalidation.** Per-node, by ADR 0008. The writer's node
  processes the invalidation. In a single-writer topology, downstream
  read replicas serve from their own L1/L2 and are subject to
  per-node staleness windows. Cluster-wide invalidation propagation
  is its own future ADR; do not assume it.
- **Failover.** Cache state on a failed node is regenerable from
  origin on the surviving node. Backups are opt-in (cache
  comparison guide § "Per-namespace byte limits, replication,
  backup"); the default is "skip; refill on first miss".

If your Redis migration is explicitly chasing cluster-mode horizontal
scale, stop here. Blob Cache is not the right target for that
workload. If your Redis migration is moving off cluster-mode toward
a single-engine deployment, Blob Cache fits.

---

## Migration playbook

The rollout is three phases. Each phase has a clear exit criterion;
do not advance past one until the criterion is met.

### Tool status

`red migrate-from-redis` is not implemented. This guide is the current
migration surface: applications own the dual-write helper, shadow-read
comparison, cutover flag, and decommission steps. A CLI that automates
the same phases is split to local follow-up #347 rather than implied by
the guide.

### Phase 1 — Dual-write

Writes go to both Redis **and** Blob Cache. Reads continue from
Redis. Blob Cache is shadow.

What you are validating in this phase:

- Blob Cache write throughput sustains your real write rate.
- L2 byte capacity is sized correctly for your working set. Watch
  `bytes_in_use` and L2 eviction counters in `BlobCache::stats`.
- Dependency / tag declarations on writes match the invalidation
  intent on reads (otherwise reads in Phase 2 will hit stale data).
- Hard TTL semantics do not bite your hottest keys. If they do, see
  the TTL section above and adjust before cutover.

Optional but recommended: **shadow reads.** On a sampled fraction of
read traffic, fetch from both Redis and Blob Cache, compare bytes,
log mismatches. Mismatches in this phase mean either your dual-write
is racy or your invalidation is incomplete; both are easier to fix
before flipping read traffic.

Exit criterion: 24 hours of clean shadow-read comparison at production
write rate, with steady-state L2 size under budget.

### Phase 2 — Cutover

Flip read traffic to Blob Cache. Redis becomes a write-only fallback
that you can flip back to in an emergency.

What you are validating:

- Hit rate on Blob Cache matches or beats Redis at the same memory
  budget. If it does not, the L1 sizing or admission policy is
  wrong; check L1 capacity and L1 admission settings in the policy
  before assuming the cache is structurally worse.
- p50 / p99 read latency is within the envelope set by your design
  point. Blob Cache is not chasing memory-only sub-ms p99 (cache
  comparison guide § "How to read the table"); make sure your
  service-level objective accounts for the L1 + L2 + (optional)
  origin path.
- Invalidation coverage: a write that should invalidate cached
  reads actually does so within your tolerance window.

Keep the cutover behind a feature flag. The flag should be a single
boolean per read site, not a global kill switch. If a particular
read path misbehaves, you can flip just that one back to Redis
without rolling the whole migration.

Exit criterion: one full business cycle (typically a week)
post-cutover with no flag flips, plus on-call sign-off.

### Phase 3 — Decommission

Stop writes to Redis. Tear down the Redis instance.

Order matters:

1. Remove the dual-write code path (writes only to Blob Cache).
2. Run for at least 24h to confirm Phase 2's monitoring still holds
   without the Redis write feeding it.
3. Stop the Redis instance. Keep the *config* in source for two
   weeks so you can spin Redis back up if a regression surfaces; do
   not delete the config in the same change as the teardown.
4. After the soak window, delete config and infrastructure.

Exit criterion: Redis off; no cache-related rollback in the soak
window; backup / monitoring / dashboards updated to no longer
reference Redis.

---

## Code samples

These samples are illustrative. The Rust sample uses the real
`BlobCache` Interface from
`crates/reddb-server/src/storage/cache/blob/cache.rs`. The JS/TS sample
uses a forward-looking `@reddb-io/sdk` shape that mirrors the
internal Interface; the public SDK surface is deferred until the
internal Interface has soaked (ADR 0006 § "Rollout"), so treat the
TypeScript binding names as indicative until the SDK ships.

### JS/TS — dual-write helper + cutover flag

```ts
import Redis from "ioredis";
import { BlobCache, BlobCachePolicy } from "@reddb-io/sdk";

const redis = new Redis(process.env.REDIS_URL!);
const blob = new BlobCache({ namespace: "session-cache" });

const READ_FROM_BLOB = process.env.READ_FROM_BLOB === "true";

export async function cachePut(
  key: string,
  value: Buffer,
  ttlSeconds: number,
): Promise<void> {
  const ttlMs = ttlSeconds * 1000;
  await Promise.all([
    redis.set(key, value, "EX", ttlSeconds),
    blob.put("session-cache", key, value, new BlobCachePolicy({ ttl_ms: ttlMs })),
  ]);
}

export async function cacheGet(key: string): Promise<Buffer | null> {
  if (READ_FROM_BLOB) {
    const hit = await blob.get("session-cache", key);
    return hit ? hit.bytes : null;
  }
  const value = await redis.getBuffer(key);
  return value ?? null;
}

export async function cacheDel(key: string): Promise<void> {
  await Promise.all([
    redis.del(key),
    blob.invalidateKey("session-cache", key),
  ]);
}
```

The `READ_FROM_BLOB` flag is the cutover gate from Phase 2. Flip it
per service (or per route) rather than globally. Writes stay
dual-target until Phase 3 removes the Redis half.

### Rust — dual-write helper using `BlobCache` directly

```rust
use std::sync::Arc;
use redis::{Client, Commands};
use reddb_server::storage::cache::blob::{
    BlobCache, BlobCachePolicy, BlobCachePut,
};

pub struct DualWriteCache {
    redis: Client,
    blob: Arc<BlobCache>,
    namespace: String,
}

impl DualWriteCache {
    pub fn new(redis: Client, blob: Arc<BlobCache>, namespace: impl Into<String>) -> Self {
        Self { redis, blob, namespace: namespace.into() }
    }

    pub fn put(&self, key: &str, value: Vec<u8>, ttl_secs: u64) -> anyhow::Result<()> {
        // Redis write.
        let mut conn = self.redis.get_connection()?;
        let _: () = conn.set_ex(key, value.clone(), ttl_secs as usize)?;

        // Blob Cache write.
        let policy = BlobCachePolicy::default().ttl_ms(ttl_secs * 1_000);
        let put = BlobCachePut::new(value).with_policy(policy);
        self.blob.put(&self.namespace, key, put)?;
        Ok(())
    }

    pub fn invalidate(&self, key: &str) -> anyhow::Result<()> {
        let mut conn = self.redis.get_connection()?;
        let _: () = conn.del(key)?;
        self.blob.invalidate_key(&self.namespace, key);
        Ok(())
    }
}
```

In Phase 2 the read path swaps to `self.blob.get(&self.namespace,
key)` behind the same flag pattern as the JS sample. In Phase 3 the
Redis half disappears and `DualWriteCache` collapses into a thin
wrapper over `BlobCache` that the rest of the application can keep
calling without changing call sites.

---

## Out of scope

Listing the things this guide deliberately does **not** cover, so
nobody has to re-derive the boundary later:

- **Pub/sub.** No Blob Cache equivalent. See § "Pub/sub absence".
- **Sorted sets, streams, hashes, lists.** Different data model. Use
  KV, RedDB queues, or keep Redis for these specific workloads.
- **Lua scripting.** Not supported; no migration target.
- **Redis modules ecosystem.** Out of scope; running both engines is
  the answer when you depend on a specific module.
- **Cluster-mode hash slots.** Not a Blob Cache target. See §
  "Cluster-mode".

---

## Cross-links

- [ADR 0006 — Tiered Blob Cache](../adr/0006-tiered-blob-cache.md)
- [`docs/guides/cache-comparison.md`](./cache-comparison.md)
- [`docs/perf/blob-cache-bench-2026-05-06.md`](../perf/blob-cache-bench-2026-05-06.md)
- [`docs/perf/blob-cache-l2-spike.md`](../perf/blob-cache-l2-spike.md)
- [ADR 0008 — Topology Advertisement & Security](../adr/0008-topology-advertisement-security.md)
