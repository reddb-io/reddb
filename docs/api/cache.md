# Remote Blob Cache

The native Blob Cache is available to remote applications over HTTP. It stores
opaque bytes by namespace and key, independently of SQL collections and the
runtime's internal result-cache namespace. Reads return bytes or a cache miss.

```js
import { connect } from '@reddb-io/client'

const db = await connect('https://database.example', {
  auth: { token: process.env.REDDB_TOKEN },
})
const namespace = 'stg:organization-id:bigquery'
const key = 'daily-usage:2026-10:query-and-parameters-hash'
const cached = await db.cache.getEntry(namespace, key)
if (cached !== null) {
  console.log(JSON.parse(new TextDecoder().decode(cached.value)))
  console.log(`expires in ${cached.ttl_ms} ms`)
} else {
  const result = await queryBigQuery()
  await db.cache.put(namespace, key, JSON.stringify(result), {
    ttl_ms: 600_000,
    tags: ['daily-usage'],
  })
}
await db.close()
```

Authorize the application user and organization before looking up cached
results. Include the environment, organization, query version and parameters
in the key; add user identity when the result is personalized. External
BigQuery changes do not trigger RedDB table dependency invalidation: choose a
TTL or explicitly refresh/invalidate the derived result.

| SDK operation | HTTP endpoint |
| --- | --- |
| `get(namespace, key)` | `GET /cache/ns/:namespace/:key` |
| `getEntry(namespace, key)` | `GET /cache/ns/:namespace/:key` |
| `getMany(namespace, keys)` | `POST /cache/ns/:namespace/mget` |
| `put(namespace, key, bytes, options)` | `PUT /cache/ns/:namespace/:key` |
| `putMany(namespace, entries)` | `POST /cache/ns/:namespace/mput` |
| `exists(namespace, key)` | `GET /cache/ns/:namespace/:key/exists` |
| `expire(namespace, key, ttlMs)` | `PATCH /cache/ns/:namespace/:key/ttl` |
| `expireAt(namespace, key, unixMs)` | `PATCH /cache/ns/:namespace/:key/ttl` |
| `persist(namespace, key)` | `PATCH /cache/ns/:namespace/:key/ttl` |
| `invalidate(namespace, key)` | `DELETE /cache/ns/:namespace/:key` |
| `invalidatePrefix(namespace, prefix)` | `DELETE /cache/ns/:namespace?prefix=...` |
| `invalidateTags(namespace, tags)` | `POST /cache/ns/:namespace/invalidate-tags` |
| `flushNamespace(namespace)` | `POST /cache/ns/:namespace/flush` |

Path components are percent-encoded by the SDK. Arbitrary key names, including
`tags`, `flush` and `invalidate-tags`, remain usable. JSON bodies carry binary
values as standard base64. A hit includes `value`, `size_bytes`, `ttl_ms`,
`expires_at_unix_ms`, `version` and `stale`; a miss returns those fields with a
null value. `get()` preserves the small value-only API, while `getEntry()` and
`getMany()` expose the metadata. Responses use `Cache-Control: no-store`.

Every route also has a canonical `/v1/cache/ns/...` alias with the same
authentication, namespace partition and behavior.

The authenticated credential determines the tenant partition; a tenant header
cannot select another partition. With IAM enabled, authorize `cache:read`,
`cache:write` and `cache:invalidate` against `cache:<namespace>` resources.
Read-only credentials cannot write or invalidate values. Application flushes
do not require or use an operator admin token.

`ttl_ms` is a hard lifetime in milliseconds, with no time-based expiry when
omitted. `expire`, `expireAt` and `persist` change an existing key's deadline
without rewriting the value. The optional `policy` accepts `ttl_ms`, `expires_at_unix_ms`,
`max_blob_bytes`, `l1_admission` (`auto`, `always`, `never`), `priority` (0–255)
and monotonically increasing `version`. Conflicting TTL fields are rejected.
Idle, stale-serving and jitter policies are not exposed by these HTTP methods.

Remote values are limited to 1 MiB, keys to 4096 UTF-8 bytes, namespaces and
tags to 256 UTF-8 bytes, and each write/invalidation to 64 tags. A batch accepts
1–256 items and at most 8 MiB of decoded values. A batch write is validated in
full before it starts, but it is not transactional: a storage or version error
during execution may leave an earlier item stored. Native cache capacity limits
also apply. Malformed requests return 400; oversized values return 413, version
conflicts 409, namespace exhaustion 429 and storage unavailability 503.

## Capacity and expiration maintenance

The server cache is bounded by a conservative estimate of resident L1 memory.
The estimate includes values, keys, metadata, tags, dependency-index key copies,
and stable container overhead. Set the global limit, per-namespace limit, and
namespace count with environment variables before start:

```sh
REDDB_CACHE_BLOB_L1_BYTES_MAX=536870912
REDDB_CACHE_BLOB_L2_BYTES_MAX=4294967296
REDDB_CACHE_BLOB_NAMESPACE_BYTES_MAX=67108864
REDDB_CACHE_BLOB_MAX_NAMESPACES=1024
```

The equivalent persisted keys are `cache.blob.l1_bytes_max`,
`cache.blob.l2_bytes_max`, `cache.blob.namespace_bytes_max`, and
`cache.blob.max_namespaces`; they apply on the next boot. Set
`cache.blob.l2_bytes_max` (or its environment variable) to `0` to disable L2.
Both memory limits are capped by RedDB's process-wide memory budget, and the
namespace limit cannot exceed the effective global limit.
Expired entries are selected from per-shard deadline indexes, so a pass costs
in proportion to due TTL records rather than total cache size. Namespace flush
reclamation uses a separate bounded generation scan only while a flush is
pending. The dedicated worker runs every 250 ms (at most 512 inspected records
or 5 ms per pass), so maintenance does not occupy query workers or perform a
stop-the-world scan.

`GET /metrics` exposes aggregate hit/miss, insert, invalidation, eviction,
current bytes/entries/namespaces, and the effective L1 and namespace limits.
The `namespace="__all__"` label is intentional: current counters are global,
not per-namespace measurements.

The memory estimate is intentionally conservative and stable; it is not an
allocator-specific RSS measurement. Keep normal process headroom for allocator
fragmentation and temporary request buffers.

The server uses a process-local L1 and a durable L2 stored in the hidden
`red.cache.blob_l2` collection inside the same `.rdb` file. Eligible values can
therefore be rehydrated after restart without an L2 sidecar. L2 defaults to a
4 GiB payload budget; L1 remains the latency tier. Expired L2 values are never
returned. Their persisted deadlines feed the same bounded maintenance worker,
so unread expired values are deleted without a steady-state collection scan;
older checkpoints rebuild a missing deadline projection in bounded ticks.
Reclaimed pages become available for later database writes. Cache data is
still derived and must not be the authoritative copy of data.

The cache and its invalidations are local to the addressed server. Use a
single cache endpoint or explicit sticky routing when deploying multiple nodes;
these routes do not add cluster-wide cache coherence.

Verify the real remote driver against a built server:

```sh
RED_BIN=/absolute/path/to/red node scripts/test-remote-cache.mjs
```
