# Remote Backends (S3, R2, Turso, D1, FS, HTTP)

RedDB supports remote storage backends for serverless / replicated
deployments. The backend stores snapshots, WAL segments, the unified
`MANIFEST.json`, and (when enabled) the writer lease.

## Available Backends

| Backend | Feature flag | `RED_BACKEND=` | Conditional writes (CAS) |
|---------|--------------|----------------|--------------------------|
| Local filesystem | (always on) | `fs` | Content-hash + exclusive `flock` |
| S3 / R2 / MinIO / DO Spaces | `backend-s3` | `s3` | ETag + `If-Match` on PUT/DELETE |
| Generic HTTP | `backend-http` | `http` | Opt-in via `RED_HTTP_CONDITIONAL_WRITES=true` |
| Turso (libSQL) | `backend-turso` | `turso` | n/a — single-writer by construction |
| Cloudflare D1 | `backend-d1` | `d1` | n/a — single-writer by construction |

Pick the backend at runtime via `RED_BACKEND=...`; per-backend
credentials and tuning use the `RED_<BACKEND>_*` family.

## Conditional-write contract

Backends that implement conditional writes power three core safety
features:

1. **Writer lease** — `RED_LEASE_REQUIRED=true` only works on a
   backend with CAS. The lease object is created/updated/released
   with a precondition; a stale heartbeat loses the race and the
   instance fails closed.
2. **Atomic manifest swap** — the unified `MANIFEST.json` is written
   with `If-Match` against the previous ETag/hash. Concurrent
   writers can never trample each other's catalog.
3. **Snapshot + WAL upload** — segments use precondition headers so
   re-uploads of the same key with different content fail loudly
   instead of overwriting silently.

`AtomicRemoteBackend` (`src/storage/backends/atomic.rs`) is the
shared trait surface. Backends that don't implement CAS still work
for plain backup/restore but cannot enforce the writer lease — set
`RED_LEASE_REQUIRED=false` (or omit it) and accept that the
operator is responsible for serialising writers.

## S3 / R2 / MinIO / DO Spaces

```bash
export RED_BACKEND=s3
export RED_S3_BUCKET=reddb-prod
export RED_S3_PREFIX=us-east-1/cluster-a
export RED_S3_REGION=us-east-1
export RED_S3_ACCESS_KEY_ID=AKIA...
export RED_S3_SECRET_KEY_FILE=/run/secrets/s3-secret
# MinIO / R2: set the endpoint
export RED_S3_ENDPOINT=https://abc.r2.cloudflarestorage.com
export RED_S3_PATH_STYLE=true                  # MinIO and a few R2 setups need this
red server --path /var/lib/reddb/data.rdb --http-bind 0.0.0.0:8080
```

Compatible with AWS S3, Cloudflare R2, MinIO, DigitalOcean Spaces,
Backblaze B2 (with `path_style=true`), Wasabi, and any service that
honours the standard S3 ETag + `If-Match` semantics.

## Local filesystem

Use a host volume or PVC. The backend keeps a content-hash CAS file
so the writer-lease contract still works without an external service.

```bash
export RED_BACKEND=fs
export RED_FS_PATH=/var/lib/reddb/backend
red server --path /var/lib/reddb/data.rdb --http-bind 0.0.0.0:8080
```

The directory needs to be writable by the RedDB process and durable
across restarts. Don't point this at NFS / EFS — POSIX advisory
locks misbehave there. (Use S3 instead for Lambda + EFS.)

## Generic HTTP backend

For appliances with a custom object-store API:

```bash
export RED_BACKEND=http
export RED_BACKEND_HTTP_URL=https://internal-store.example.com/reddb
export RED_BACKEND_HTTP_AUTH_FILE=/run/secrets/http-auth
export RED_HTTP_CONDITIONAL_WRITES=true        # opt-in; required for lease
```

The endpoint must implement:

- `GET /<key>` returning `200` + body + `ETag`, or `404`.
- `PUT /<key>` honouring `If-Match: <etag>` and `If-None-Match: *`.
- `DELETE /<key>` honouring `If-Match: <etag>`.

Without `RED_HTTP_CONDITIONAL_WRITES=true` the backend runs in
plain-PUT mode (backup/restore only, no writer lease).

## Turso

```bash
export RED_BACKEND=turso
export RED_TURSO_URL=libsql://my-db.turso.io
export RED_TURSO_TOKEN_FILE=/run/secrets/turso-token
```

Single-writer by construction; the writer lease is a no-op because
Turso enforces single-writer at the libSQL layer.

## Cloudflare D1

```bash
export RED_BACKEND=d1
export RED_D1_ACCOUNT_ID=…
export RED_D1_DATABASE_ID=…
export RED_D1_TOKEN_FILE=/run/secrets/d1-token
```

Same single-writer property as Turso. Best for very small datasets
behind Cloudflare Workers — D1 is not the right choice for
multi-GB WAL traffic.

## Use Cases

| Backend | Best For |
|:--------|:---------|
| Local file | Development, single-server production with fast NVMe |
| S3 / R2 | Serverless writers, multi-region DR, the default for `RED_LEASE_REQUIRED=true` |
| Generic HTTP | On-prem appliances, custom object stores |
| Turso | Edge SQL, very small global datasets |
| D1 | Cloudflare Workers integration |

> [!WARNING]
> Remote backends add network latency to manifest writes and segment
> uploads, **not** to query reads (those serve from the local data
> file). Tune `RED_BACKUP_INTERVAL_SECS` so per-segment uploads
> happen often enough that a cold-start restore replays a small WAL
> tail.

## See also

- [Replication](replication.md) — commit policy + writer lease
- [Serverless Mode](serverless.md) — `RED_BACKEND` + `RED_AUTO_RESTORE`
- [Operator Runbook §1](../operations/runbook.md#writer-lease-backend-matrix)
- [Manifest Format](../spec/manifest-format.md)
