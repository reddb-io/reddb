# KV product-path comparison — 2026-10-20

Follow-up: [REST key-index integration](kv-rest-index-2026-10-20.md) addresses the
read-path gap below and measures the change with fresh before/after volumes.
This report retains the original engine revision and observations.

The local diagnostic does **not** demonstrate performance parity with Redis or
KiviDB. It identifies a concrete RedDB integration gap: REST GET still scans and
clones the collection, while RQL KV GET uses the physical-key index. The durable
overwrite path also needs investigation: grouped WAL durability did not remove
its throughput plateau in this capture.

These are verified product-path observations on a shared Docker Desktop/WSL2
host, not maximum engine throughput or a public competitive-lead claim. HTTP/JSON
and RESP have different protocol and response costs. The Python client can limit
throughput, and five rounds share a server process. This does not reproduce the
32-core, ten-million-key, memtier experiment behind KiviDB's headline.

## Read results

10,000 keys, 100-byte deterministic values, one persistent connection, pipeline 1.
Numbers are medians of five round-level statistics. The primary metric was GET
p99 completion latency. Every measured response was checked against its value.

| Product path | GETs/s | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: |
| Redis 8.4.0 / RESP2 | 4,902 | 0.196 | 0.233 | 0.262 |
| KiviDB 1.1.0 / RESP2 | 5,225 | 0.162 | 0.269 | 0.757 |
| RedDB / HTTP RQL, cap64 | 1,173 | 0.828 | 1.257 | 1.630 |
| RedDB / HTTP REST, cap64 | 96 | 10.038 | 13.333 | 17.158 |

At 32 connections on the same 10,000-key workload:

| Product path | GETs/s | p99 ms |
| --- | ---: | ---: |
| Redis / RESP2 | 9,837 | 10.259 |
| KiviDB / RESP2 | 8,523 | 15.144 |
| RedDB / HTTP RQL, cap64 | 2,761 | 33.262 |
| RedDB / HTTP REST, cap64 | 304 | 233.355 |

The REST path fell from 1,201 GETs/s at 1,000 keys to 96 at 10,000 keys with one
connection. RQL changed from 1,755 to 1,173 GETs/s. This is consistent with the
source-level difference; it does not isolate every source of RQL overhead.
REST reached roughly 393% server CPU at 32 connections, against its four-CPU
limit, while the client used roughly 13%. The scan is a useful optimization
target. Competitor throughput is not a measured capacity ceiling: their clients
used substantially more CPU than their servers in several concurrent cells.

The source boundary at engine revision `a3af4d62` is:

- `server/handlers_entity.rs::handle_get_kv` calls the entity application port.
- `application/ports_impls_entity.rs::get_kv` uses `manager.query_all` and named
  fields, materializing the collection before finding a matching key.
- `runtime/impl_kv.rs::get_entity` uses the segment-key index. RQL GET requests
  an owned entity for its metadata envelope. The smaller value-only projection
  is used by `KvAtomicOps::get_entry` / `get`, not by every transport.

The initial default-cap profile also returned HTTP 503 at 32 connections. On
four available CPUs, the built-in handler cap resolves to eight. That failed
cell remains in the artifacts. The separately labeled cap64 profile sets
`--http-max-handlers 64` without changing CPU, memory or durability limits.
Its 32-connection cells passed. A configurable admission limit is distinct from
engine throughput.

## Durable overwrite results

1,000 keys, 100-byte values, eight connections, five rounds of 500 logical
overwrites. Each round changes its values and checks all changed keys. The
entire final state is checked after SIGKILL and reopening the same volume.

| Product path and completion boundary | Writes/s | p99 ms |
| --- | ---: | ---: |
| Redis SET, AOF `appendfsync always` | 1,704 | 9.968 |
| KiviDB SET + WAITAOF local fsync confirmation | 1,426 | 8.760 |
| RedDB REST PUT, `strict`, cap64 | 126 | 103.526 |
| RedDB REST PUT, `sync` / WalDurableGrouped, cap64 | 126 | 77.831 |

RedDB's grouped mode also waits for WAL fsync before completing the operation.
It improved this capture's p99 but did not materially increase throughput. The
next investigation should trace mutation serialization and persistence waits.
The non-versioned replacement path performs delete plus create. A subsequent
call-path review established that ordinary REST PUT uses `KvAtomicOps::delete`,
which already uses indexed lookup. INCR/CAS use the application delete port,
whose `get_kv` scans in this revision. Removing that scan must not be assumed
to fix the measured PUT plateau; persistence waits still need profiling.

KiviDB's native SET response alone is not counted as sync-durable. Its pinned
configuration accepts `appendfsync always` with a no-effect warning while the
AOF writer syncs periodically. The timed logical operation therefore includes
SET and WAITAOF, and requires `[1, 0]` from the fence. This is an explicit
client-assisted completion boundary. The workload checks acknowledged local
persistence and final values; it does not prove equivalence for overlapping
read/write atomicity, cross-key transactions or power-loss recovery.

## Evidence and conditions

- Engine: `a3af4d6261f05fcc2dccd5caca178c5ddf6936a3`, including the pending
  value-projection change; this is not an unmodified v1.24.15 release binary.
- Optimized build: Rust 1.95.0, `cargo build --release --locked --bin red -j 4`.
  Binary SHA256: `00e800ab0494a8b24fb9e58a1f23f16e3e692ad2fdac0a288cb7b4e9895ac3e1`.
- AMD Ryzen 5 3600 host; Docker exposes 12 CPUs and about 20 GB RAM. Each server
  uses CPU set 0–3, four CPUs, a 2 GiB process limit and a 1 GiB engine budget.
  The client uses CPU set 4–5, two CPUs and 1 GiB. Other host activity remained.
- Engines ran sequentially. Container images, digests, commands, environment,
  limits and the continuation profiles are recorded in the manifest. Redis
  8.4.0 was used; this is not the Redis 8.10 comparison in the cited headline.
- 46 successful measurement cells, 14 HTTP-pipeline cells unsupported by this
  harness, one invalid default-cap cell, and 14 successful process-crash recovery
  checks. No incorrect returned value was accepted as a successful operation.
- The initial Windows Docker stats collector missed ANSI-prefixed samples.
  Early Redis 1,000-key telemetry is unavailable. A supplementary stream and
  the repaired continuation collectors preserve later samples. Zero-only CPU
  samples under active load are marked unavailable; raw counters remain visible.
  Working-set memory is not per-key memory or RSS. Network and block-I/O samples
  are retained alongside CPU and memory.

The [experiment contract and reproduction commands](../../bench/kv/COMPARE.md),
[complete tables](../../bench/results/kv-compare-2026-10-20/SUMMARY.md),
[run manifest](../../bench/results/kv-compare-2026-10-20/manifest.json) and
[round/resource summary and artifact checksums](../../bench/results/kv-compare-2026-10-20/summary.json)
make the unsuccessful cells and raw observations reviewable.

The immediate implementation priority is to share the indexed key resolver with
REST while preserving model checks, tenant/policy scope, snapshot selection and
the response envelope. Then measure that change on the same matrix and profile
the write path. This capture provides a baseline and concrete bottlenecks; it
does not select an engine rewrite or establish ADR 0076 competitive leadership.
