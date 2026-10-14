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
const payload = await db.cache.get(namespace, key)
if (payload !== null) {
  console.log(JSON.parse(new TextDecoder().decode(payload)))
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
| `put(namespace, key, bytes, options)` | `PUT /cache/ns/:namespace/:key` |
| `exists(namespace, key)` | `GET /cache/ns/:namespace/:key/exists` |
| `invalidate(namespace, key)` | `DELETE /cache/ns/:namespace/:key` |
| `invalidatePrefix(namespace, prefix)` | `DELETE /cache/ns/:namespace?prefix=...` |
| `invalidateTags(namespace, tags)` | `POST /cache/ns/:namespace/invalidate-tags` |
| `flushNamespace(namespace)` | `POST /cache/ns/:namespace/flush` |

Path components are percent-encoded by the SDK. Arbitrary key names, including
`tags`, `flush` and `invalidate-tags`, remain usable. JSON bodies carry binary
values as standard base64; misses return `{ "value": null }` with HTTP 200.
Responses use `Cache-Control: no-store`.

The authenticated credential determines the tenant partition; a tenant header
cannot select another partition. With IAM enabled, authorize `cache:read`,
`cache:write` and `cache:invalidate` against `cache:<namespace>` resources.
Read-only credentials cannot write or invalidate values. Application flushes
do not require or use an operator admin token.

`ttl_ms` is a hard lifetime in milliseconds, with no time-based expiry when
omitted. The optional `policy` accepts `ttl_ms`, `expires_at_unix_ms`,
`max_blob_bytes`, `l1_admission` (`auto`, `always`, `never`), `priority` (0–255)
and monotonically increasing `version`. Conflicting TTL fields are rejected.
Idle, stale-serving and jitter policies are not exposed by these HTTP methods.

Remote values are limited to 1 MiB, keys to 4096 UTF-8 bytes, namespaces and
tags to 256 UTF-8 bytes, and each write/invalidation to 64 tags. Native cache
capacity limits also apply. Malformed requests return 400; oversized values
return 413, version conflicts 409, namespace exhaustion 429 and storage
unavailability 503.

The cache and its invalidations are local to the addressed server. Use a
single cache endpoint or explicit cache routing when deploying multiple nodes;
these routes do not add cluster-wide cache coherence. Native L1/L2 admission,
eviction and persistence behavior continues to apply.

Verify the real remote driver against a built server:

```sh
RED_BIN=/absolute/path/to/red node scripts/test-remote-cache.mjs
```
