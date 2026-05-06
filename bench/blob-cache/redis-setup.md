# Redis baseline setup for the Blob Cache bench

This file pins the **exact** Redis docker invocations the Blob
Cache bench suite uses for its baseline rows. Reproducing the
report numbers requires running these commands verbatim — image
tag, args, ports, persistence config.

The two variants exist because RedDB's Blob Cache pays for L2
durability ordering on some scenarios; comparing only against
`no-persist` Redis would hide that cost, and comparing only
against `aof-everysec` Redis would hide the apex Redis throughput.
Both rows live in the report.

## Pin

| field | value |
|-------|-------|
| image | `redis:7.4` |
| platform | `linux/amd64` (explicit; avoids an Apple Silicon mismatch when re-running on a Mac host) |
| port | `6379/tcp`, bound to `127.0.0.1` only |
| transport | localhost loopback TCP (no unix socket variant) |

The image digest should be captured in the session-id rollup
(`docker image inspect redis:7.4 --format '{{ .Id }}'`) so the
`Cited session` line in
[`docs/perf/blob-cache-bench-2026-05-06.md`](../../docs/perf/blob-cache-bench-2026-05-06.md)
can be replayed against the exact image bytes the bench saw.

## Variant 1 — `redis-no-persist`

Memory-only Redis. Apex throughput baseline. Used by:

- workload 1 (`hot-l1-hit`)
- workload 3 (`cold-absent`)
- workload 4 (`large-blob-l2-hit`) — `no-persist` cell
- workload 5 (`namespace-flush`)
- workload 6 (`dependency-invalidation`)
- workload 8 (`mixed-blob admission`) — `allkeys-lru` cell

```bash
docker run -d --rm \
  --name reddb-bench-redis-no-persist \
  --platform linux/amd64 \
  -p 127.0.0.1:6379:6379 \
  redis:7.4 \
  redis-server \
    --save "" \
    --appendonly no \
    --maxmemory 1gb \
    --maxmemory-policy allkeys-lru
```

Flags rationale:

- `--save ""` disables RDB snapshotting entirely.
- `--appendonly no` disables the AOF.
- `--maxmemory 1gb` matches the working-set ceiling the
  scenarios drive — large enough for every workload that uses
  this variant, small enough that `allkeys-lru` actually engages
  on workload 8.
- `--maxmemory-policy allkeys-lru` is the eviction policy
  workload 8 compares SIEVE / W-TinyLFU against.

## Variant 2 — `redis-aof-everysec`

Durability-comparable variant. Used by:

- workload 1 (`hot-l1-hit`) — `aof-everysec` cell
- workload 2 (`cold-l2-miss`) — only this variant; `no-persist`
  Redis has no L2-equivalent
- workload 4 (`large-blob-l2-hit`) — `aof-everysec` cell
- workload 7 (`restart-warm-cache`) — only this variant; the
  whole point is to measure AOF replay on restart

```bash
docker run -d --rm \
  --name reddb-bench-redis-aof-everysec \
  --platform linux/amd64 \
  -p 127.0.0.1:6380:6379 \
  -v reddb-bench-redis-aof:/data \
  redis:7.4 \
  redis-server \
    --save "" \
    --appendonly yes \
    --appendfsync everysec \
    --dir /data \
    --maxmemory 1gb \
    --maxmemory-policy allkeys-lru
```

Flags rationale:

- Bound to host port `6380` so both variants can run side by
  side. The bench harness passes the port per cell.
- `--save ""` keeps RDB snapshotting off so the only persistence
  surface is AOF — keeps the comparison narrow.
- `--appendonly yes --appendfsync everysec` is the closest
  apples-to-apples to RedDB Blob Cache's L2 durability ordering
  (metadata-last + page-aligned blob chains, group-fsync at
  segment boundaries). Both pay for "≤ 1 s of data loss on
  crash"; neither pays per-op fsync.
- `--dir /data` + the named volume `reddb-bench-redis-aof` is
  what lets workload 7 measure AOF replay across a process
  restart. The volume must persist across `docker stop` / `docker
  start`; do **not** use a tmpfs mount.

## Bring-up / tear-down

The follow-up implementation slice will wrap these in
`bench/blob-cache/redis-up.sh` and `redis-down.sh`. Until then,
run by hand:

```bash
# Bring up both variants.
docker run -d --rm --name reddb-bench-redis-no-persist \
  --platform linux/amd64 -p 127.0.0.1:6379:6379 redis:7.4 \
  redis-server --save "" --appendonly no \
  --maxmemory 1gb --maxmemory-policy allkeys-lru

docker run -d --rm --name reddb-bench-redis-aof-everysec \
  --platform linux/amd64 -p 127.0.0.1:6380:6379 \
  -v reddb-bench-redis-aof:/data redis:7.4 \
  redis-server --save "" --appendonly yes --appendfsync everysec \
  --dir /data --maxmemory 1gb --maxmemory-policy allkeys-lru

# Verify both are healthy.
docker exec reddb-bench-redis-no-persist redis-cli -p 6379 ping
docker exec reddb-bench-redis-aof-everysec redis-cli -p 6379 ping

# Tear down (the volume is left in place for workload 7 reruns).
docker stop reddb-bench-redis-no-persist
docker stop reddb-bench-redis-aof-everysec

# Wipe the AOF volume only when explicitly resetting workload 7.
docker volume rm reddb-bench-redis-aof
```

## Workload 7 specific — restart procedure

Workload 7 (`restart-warm-cache`) measures AOF replay time on
the Redis side. The exact procedure the bench must follow:

```bash
# 1. Populate phase: bench harness writes the full key set
#    against reddb-bench-redis-aof-everysec (port 6380).

# 2. Issue a final BGREWRITEAOF + wait for it to complete so
#    the on-disk AOF is in its compact form.
docker exec reddb-bench-redis-aof-everysec \
  redis-cli -p 6379 BGREWRITEAOF
# poll INFO persistence for aof_rewrite_in_progress = 0

# 3. Stop the container WITHOUT removing the volume.
docker stop reddb-bench-redis-aof-everysec

# 4. Bring it back up against the same volume; this is the
#    cell the bench times.
time docker run -d --rm \
  --name reddb-bench-redis-aof-everysec \
  --platform linux/amd64 -p 127.0.0.1:6380:6379 \
  -v reddb-bench-redis-aof:/data redis:7.4 \
  redis-server --save "" --appendonly yes --appendfsync everysec \
  --dir /data --maxmemory 1gb --maxmemory-policy allkeys-lru

# 5. Poll PING until it returns PONG; that's "open ms".
# 6. Issue one GET against a known-populated key; that's
#    "first-hit p50".
# 7. Run the 100K random reads; that's the steady-state row.
```

The corresponding RedDB-side procedure is documented in
[`scenarios.md`](scenarios.md) under scenario 7.

## What this file deliberately does not cover

- **Redis cluster mode.** The Blob Cache is single-process; the
  baseline mirrors that. A clustered Redis comparison is a
  different question (and a different bench).
- **Redis modules.** No `RedisJSON`, `RedisBloom`, `RediSearch`.
  ADR 0006 §"Redis positioning" explicitly does not target
  module ecosystem parity.
- **TLS / auth.** Loopback bind, no auth — the comparison is
  purely about cache mechanics, not network security.
- **Pipelining beyond 64 ops/batch.** The pipelined Redis cells
  use a 64-op batch size; pushing it higher hides the per-op
  cost the comparison is trying to expose. If a future workload
  legitimately wants larger pipelines, add it as a new cell, do
  not change this default.
