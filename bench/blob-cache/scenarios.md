# Blob Cache benchmark scenarios

This file pins the **eight** workloads the Blob Cache bench suite
must run. The shape of the scenarios traces directly to the
acceptance criteria in issue #149 plus one addition
(**mixed-blob admission**) that exists to drive the SIEVE vs
W-TinyLFU decision criterion documented in
[`docs/perf/blob-cache-bench-2026-05-06.md`](../../docs/perf/blob-cache-bench-2026-05-06.md).

## Common knobs

These apply to every scenario unless overridden in the scenario's
own table.

| knob | value | rationale |
|------|-------|-----------|
| `runs_per_cell` | 10 | Same as `OFFICIAL_RUNS = 10` (#154 lock). |
| `warmup_iters` | 2 | Discarded; matches `cold-start-baseline.md`. |
| `build_profile` | `--release` | No debug-build comparison cells. |
| `l1_bytes_max` | `256 MiB` (default) | `DEFAULT_BLOB_L1_BYTES_MAX` from `blob.rs`. |
| `l2_bytes_max` | `4 GiB` (default) | `DEFAULT_BLOB_L2_BYTES_MAX` from `blob.rs`. |
| `shard_count` | 64 (default) | `DEFAULT_BLOB_SHARDS`. |
| `redis_image` | `redis:7.4` | Pinned in [`redis-setup.md`](redis-setup.md). |
| `redis_variants` | `no-persist`, `aof-everysec` | Two-row baseline. |
| `redis_modes` | `single-shot`, `pipelined` | Per applicable scenario. |
| `transport` | localhost loopback TCP | Same socket discipline both sides. |

Working-set sizing convention (used by the `WS` column below):

- `WS = 0.5 × L1` — fits comfortably in L1.
- `WS = 1.0 × L1` — at-capacity.
- `WS = 2.0 × L1` — over-capacity (drives admission policy).
- `WS = 4.0 × L1 + L2 active` — drives L2 hit rate.

`L1` always means the configured `l1_bytes_max` for the run
(default 256 MiB unless the scenario overrides it).

## Scenario 1 — `hot-l1-hit`

Hot path. Single key (or tight key set) reused; working set fits
trivially in L1; drives the `Arc<[u8]>` clone + shard read path.

| knob | value |
|------|-------|
| payload size | 1 KB |
| key set size | 32 |
| op count | 1,000,000 (single-shot) / 1,000,000 (pipelined batches of 64) |
| working set | `0.5 × L1` (32 × 1 KB ≪ 256 MiB) |
| prefetch | n/a (warm by warmup) |
| l1 admission policy | `BlobCachePolicy::default()` (`Auto`) |
| compares against | `ResultCache::get_one`, Redis `GET` (single + pipelined), both Redis variants |
| metrics | ops/sec, p50 µs, p99 µs |

Why this shape: the loop should be CPU-bound on the cache hit path,
not on payload allocation. 32 distinct keys keep the SIEVE `visited`
bits exercised across multiple shard buckets.

## Scenario 2 — `cold-l2-miss`

L1 evicted, L2 hit. Drives metadata B-tree read + native blob-chain
reassembly per ADR 0006 §"L2 database store".

| knob | value |
|------|-------|
| payload size | 16 KB |
| key set size | 32,000 (working set ≈ 512 MiB > L1) |
| op count | 100,000 reads (random over key set) |
| working set | `4.0 × L1`, `L2 active` |
| prefetch | none — cold L1, warm L2 (populated in setup phase) |
| l1 admission policy | `Auto` |
| compares against | Redis `GET` (cold from disk via AOF reload variant — see Redis cell below) |
| Redis cell | `aof-everysec` only; `no-persist` is unfair (no L2-equivalent) |
| metrics | ops/sec, p50 µs, p99 µs, `l2_metadata_reads` counter delta |

Setup phase: write the full key set with `BlobCachePolicy::default()`,
then drop L1 (`l1_bytes_max` set lower for the run, or
`cache.flush_l1()` if the surface exposes it; otherwise repopulate
after a `BlobCache::new` rebind on the same L2 path).

## Scenario 3 — `cold-absent` (synopsis effectiveness)

Key never written. Membership synopsis must skip the L2 metadata
read. This is the workload that proves ADR 0006 §"Membership
synopsis" delivers.

| knob | value |
|------|-------|
| payload size | n/a (no put — every read is a miss) |
| key set size | 100,000 keys, all unwritten |
| op count | 1,000,000 reads |
| working set | n/a |
| prefetch | n/a |
| l1 admission policy | n/a |
| compares against | `ResultCache` miss path, Redis `GET` returning `nil` (both variants, single-shot + pipelined) |
| metrics | ops/sec, p50 µs, p99 µs, `l2_negative_skips / misses` ratio |

The headline number for this scenario is the **synopsis-skip rate**:
`l2_negative_skips / misses`. If it is not ≥ 0.95 the synopsis is
not earning its keep and the design needs revisiting before any
public API design starts.

## Scenario 4 — `large-blob-l2-hit` (5 MiB)

Single 5 MiB blob, L1 cold, L2 warm. Stresses native blob-chain
read + `Arc<[u8]>` materialisation cost. This is the row where
Redis is most likely to win on raw memcpy.

| knob | value |
|------|-------|
| payload size | 5 MiB |
| key set size | 64 (≈ 320 MiB working set, exceeds L1) |
| op count | 5,000 reads |
| working set | `1.25 × L1`, `L2 active` |
| prefetch | populated in setup phase |
| l1 admission policy | `L1Admission::Always` for one cell, `L1Admission::Never` for another (compare both) |
| compares against | Redis `GET` of a 5 MiB string (both variants, single-shot only — pipelining 5 MiB reads is unrealistic) |
| metrics | MB/sec, p50 ms, p99 ms |

Why two L1-admission cells: 5 MiB blobs are exactly the case where
`L1Admission::Auto` may decide to skip L1; pinning both ends of
the policy makes the report honest about the tradeoff.

## Scenario 5 — `namespace-flush`

`cache.flush(namespace)` on a populated namespace. Per ADR 0006
§"Invalidation", the foreground path is an O(1) generation bump.
Sweeper-side reclamation is measured separately.

| knob | value |
|------|-------|
| payload size | 4 KB |
| key set size | 50,000 (≈ 200 MiB; fits in L1) |
| op count | 50 flush calls (one per repopulate) |
| working set | `0.8 × L1` |
| prefetch | full namespace populated before each flush |
| l1 admission policy | `Auto` |
| compares against | Redis `FLUSHDB` (one cell), Redis `SCAN`+`DEL` for keyspace prefix (one cell) |
| metrics | foreground flush p50 ms, p99 ms, sweeper reclamation lag p50 ms, `namespace_flushes` counter delta |

The "sweeper reclamation lag" is the wall-clock from the flush
call returning to the point at which `entries == 0` for the
flushed namespace. RedDB-only metric (Redis has no equivalent —
its DEL is synchronous).

## Scenario 6 — `dependency-invalidation`

`invalidate_dependencies(["table:users"])` on a populated namespace
where N% of entries carry that dependency tag. Drives the
dependency-index path from ADR 0006 §"Invalidation".

| knob | value |
|------|-------|
| payload size | 4 KB |
| key set size | 100,000 |
| op count | 50 invalidation calls (one per repopulate) |
| working set | `1.5 × L1` |
| dependency tag distribution | 25% of entries carry `table:users` |
| prefetch | full key set populated before each call |
| l1 admission policy | `Auto` |
| compares against | `ResultCache::invalidate_dependent_caches`, Redis Lua-based tag-set sweep |
| metrics | invalidation count returned, p50 ms, p99 ms, per-invalidated-entry cost (call ms / count) |

## Scenario 7 — `restart-warm-cache`

Process restart against a populated L2. Measures time-to-first-hit
and steady-state hit rate after restart. This is the headline
durability differentiator vs Redis.

| knob | value |
|------|-------|
| payload size | 8 KB |
| key set size | 200,000 (≈ 1.6 GiB, well within L2 default of 4 GiB) |
| op count post-restart | 100,000 reads (random over the populated key set) |
| working set | `6.4 × L1`, `L2 active` |
| prefetch | full key set populated before stop; process restarted; cache reopened on the same L2 path |
| l1 admission policy | `Auto` |
| compares against | Redis cold-start under `appendonly yes` (AOF replay) — `aof-everysec` variant only |
| metrics | open ms, first-hit p50 µs, post-restart `entries` reachable, post-restart hit rate over the 100K reads |

Setup is two-phase:

1. **populate** — start the process, write the full 200K key set,
   issue `cache.sweep_expired(0)` to flush any pending state, stop
   the process cleanly (drop the `BlobCache` to flush L2).
2. **measure** — start a fresh process pointed at the same L2
   path, time the open call (`open ms`), issue one read against a
   known-populated key (`first-hit p50`), then run the 100K random
   reads (steady-state hit rate).

The Redis cell mirrors this: `redis-cli SHUTDOWN`, then bring the
same volume back up under `aof-everysec` and measure AOF replay
time + first `GET` latency.

## Scenario 8 — `mixed-blob admission` (1 KB / 100 KB / 5 MiB)

Randomised mix of small / medium / large blobs into a fixed L1
budget. **This is the SIEVE vs W-TinyLFU oracle** — the row that
decides whether the L1 admission policy migrates per the criterion
in the bench report.

| knob | value |
|------|-------|
| payload size mix | 70 % × 1 KB, 25 % × 100 KB, 5 % × 5 MiB |
| key set size | sized so total bytes ≈ `2.0 × L1` |
| op count | 500,000 mixed `put` / `get` (80/20 read/write) |
| working set | `2.0 × L1` (the policy-divergence regime) |
| prefetch | none — measure cold-fill admission behaviour |
| l1 admission policy | one cell per policy: `SIEVE` (shipped), `W-TinyLFU` (if flagged), `allkeys-lru` (Redis side) |
| compares against | Redis `SET` / `GET` with the same mix; `ResultCache` for the 1 KB slice only (it is not a blob store) |
| metrics | L1 hit rate, eviction count, `put` p50 µs, `put` p99 µs |

This scenario also runs at `WS = 0.5 × L1` and `WS = 1.0 × L1` as
sanity rows; the policy-divergence call is made at `WS = 2.0 × L1`.

### Decision rule (re-stated)

If, at `WS = 2.0 × L1`, **W-TinyLFU's hit rate beats SIEVE's hit
rate by more than 5 percentage points** under either persistence-
off or `appendfsync everysec` Redis baselines, file a follow-up
issue to migrate L1 to W-TinyLFU. Otherwise the ADR-pinned SIEVE
default holds.

The full reasoning for the 5 pp threshold lives in the bench
report's "SIEVE vs W-TinyLFU decision criterion" section.

## Per-scenario cell matrix (summary)

| # | scenario | RedDB cells | Redis cells | other cells |
|--:|----------|------------:|------------:|------------:|
| 1 | hot-l1-hit | 1 (BlobCache L1) | 4 (2 variants × 2 modes) | 1 (`ResultCache`) |
| 2 | cold-l2-miss | 1 | 1 (`aof-everysec` only) | 0 |
| 3 | cold-absent | 1 | 4 (2 variants × 2 modes) | 1 (`ResultCache`) |
| 4 | large-blob-l2-hit | 2 (`Always`, `Never`) | 2 (2 variants, single-shot) | 0 |
| 5 | namespace-flush | 1 | 2 (`FLUSHDB`, `SCAN`+`DEL`) | 0 |
| 6 | dependency-invalidation | 1 | 1 (Lua tag-set) | 1 (`ResultCache`) |
| 7 | restart-warm-cache | 1 | 1 (`aof-everysec` AOF replay) | 0 |
| 8 | mixed-blob admission | 1–2 (SIEVE; W-TinyLFU if flagged) | 1 (`allkeys-lru`) | 1 (`ResultCache`, 1 KB slice only) |

Total rows in the report tables: **≈ 30** (give or take the
optional W-TinyLFU cell).
