# Blob Cache benchmark suite — scaffold report — 2026-05-06

Status: **scaffold only** (no benches executed yet). This file is the
report skeleton with workload definitions, baseline configuration, the
SIEVE → W-TinyLFU migration trigger, and placeholder result tables.
The `TBD` cells are filled by re-running the harness described under
"Reproducing" once the bench binaries land — see "What this scaffold
intentionally leaves out" for the follow-up slice.

Tracking issue: #149 — *"Blob Cache: benchmark impact against result
cache and Redis"*.

Parent issue: #139.

ADR: [`0006-tiered-blob-cache.md`](../adr/0006-tiered-blob-cache.md).
Spike doc: [`blob-cache-l2-spike.md`](blob-cache-l2-spike.md).

Cited session id slot: `sess-canonical-pending` (real session id is
written here once the bench actually runs against the canonical
config; until then this string is the sentinel).

## TL;DR (placeholder until first run)

- L1 hot hit p50 vs result-cache p50: **TBD**.
- L1 hot hit p50 vs Redis (loopback, pipelined GET) p50: **TBD**.
- Cold-absent (synopsis-effective) p50 vs Redis MISS p50: **TBD**.
- L2 hit (5 MiB blob) throughput vs Redis GET (5 MiB) throughput:
  **TBD**.
- SIEVE vs W-TinyLFU hit-rate gap on the mixed-blob workload:
  **TBD pp**. Migration trigger fires at **> 5 pp**; below that, ship
  SIEVE per ADR 0006 §"L1 memory".

## Setup

### Hardware / OS pin

The canonical bench host is the same one used by `make duel-official`
in [`rdb-benchmark`](https://github.com/reddb-io/rdb-benchmark) under
issue #154. Re-bench on a different host requires a fresh session id;
do not mix sessions across hosts. The exact host string is captured
in the session header when the bench runs.

### RedDB build pin

- Toolchain: `rust-toolchain.toml` from this worktree.
- Profile: `--release`.
- Features: default workspace features. Blob cache surface is
  `crates/reddb-server/src/storage/cache/blob.rs` with the constants
  `DEFAULT_BLOB_L1_BYTES_MAX = 256 MiB` and
  `DEFAULT_BLOB_L2_BYTES_MAX = 4 GiB` left at their defaults unless a
  scenario in [`scenarios.md`](../../bench/blob-cache/scenarios.md)
  overrides them.
- Result-cache surface compared against:
  `crates/reddb-server/src/storage/cache/result.rs` (`ResultCache`).

### Redis baseline pin

- Image: `redis:7.4` (single-instance, default config). The exact
  pinned tag and full docker invocation lives in
  [`bench/blob-cache/redis-setup.md`](../../bench/blob-cache/redis-setup.md).
- Two persistence variants are run, both on the same loopback TCP
  socket the bench client uses:
  - `redis-no-persist` — `--save "" --appendonly no` (memory-only,
    apex throughput baseline).
  - `redis-aof-everysec` — `--appendonly yes --appendfsync everysec`
    (durability-comparable variant for the L2 / restart-warm
    scenarios where the RedDB blob cache is paying for fsync ordering
    too).
- Transport: localhost loopback TCP (`127.0.0.1:6379`). No unix
  socket variant — keeps the comparison apples-to-apples with the
  RedDB embedded gRPC / native call path which also crosses a
  socket-or-equivalent boundary at the public API layer.
- Two client modes per scenario:
  - **Single-shot** — one RTT per `GET` / `SET`. Models the
    application-cache call shape.
  - **Pipelined** — batched `MGET` / `MSET` matching the workload's
    natural fan-out. Models the upper bound where Redis dominates by
    amortising network cost.

### Bench harness pin

The canonical methodology lock is `make duel-official` in
`rdb-benchmark/` (issue #154). The Blob Cache suite is a sibling
harness with the same session-id discipline:

- Working dir: `bench/blob-cache/` (this scaffold).
- Run discipline: see
  [`bench/blob-cache/README.md`](../../bench/blob-cache/README.md).
- Per-scenario parameters: see
  [`bench/blob-cache/scenarios.md`](../../bench/blob-cache/scenarios.md).
- Per-cell run count: 10 (same as `OFFICIAL_RUNS = 10`).
- Per-run warmup: discarded first 2 iterations (matches the
  `cold-start-baseline.md` discipline).

## Workloads

The eight scenarios below come straight from the issue #149
acceptance list, with one addition: **mixed-blob admission**, which
exists to drive the SIEVE vs W-TinyLFU decision criterion further
down this doc. Full parameters per scenario are formalised in
[`bench/blob-cache/scenarios.md`](../../bench/blob-cache/scenarios.md);
the rows here are the bench-report summary.

| # | Workload | What it pins | RedDB target | Compared against |
|--:|----------|--------------|--------------|------------------|
| 1 | **hot-l1-hit** | Hot path: `get(namespace, key)` against a working set that fits L1. Drives `BlobCache` `Arc<[u8]>` clone path. | L1 hit p50 / p99, ops/sec | `ResultCache::get_one` (same key reused), Redis `GET` single-shot + pipelined |
| 2 | **cold-l2-miss** | L1 evicted, L2 hit. Drives the metadata B-tree read + native blob-chain reassembly. | L2 hit p50 / p99, ops/sec | `ResultCache` (cold — has no L2, so this is a population vs read comparison), Redis `GET` (cold from disk via AOF reload variant) |
| 3 | **cold-absent (synopsis effectiveness)** | Key never written. Membership synopsis must skip the L2 metadata read. | Negative-skip rate (`l2_negative_skips / misses`), miss p50 / p99 | `ResultCache` miss path, Redis `GET` returning `nil` |
| 4 | **large-blob-l2-hit (5 MiB)** | Single 5 MiB blob, L1 cold, L2 warm. Stresses native blob-chain read + `Arc<[u8]>` materialisation cost. | Throughput MB/s, p50 / p99 latency | Redis `GET` of a 5 MiB string (both persistence variants) |
| 5 | **namespace-flush** | `cache.flush(namespace)` on a populated namespace. Per ADR 0006, this is a generation bump on the foreground path. | Foreground p50 / p99 of the flush call; sweeper-side reclamation lag | Redis `FLUSHDB` (and `SCAN`+`DEL` for keyspace-prefix variant) |
| 6 | **dependency-invalidation** | `invalidate_dependencies(["table:users"])` on a populated namespace where N% of entries carry that dependency tag. | Invalidation count returned, p50 / p99 of the call, fanout cost per invalidated entry | `ResultCache::invalidate_dependent_caches(...)`, Redis (script-based tag invalidation via secondary set lookup) |
| 7 | **restart-warm-cache** | Process restart against a populated L2. Measures time-to-first-hit and steady-state hit rate after restart. | Cold-open ms; `entries` reachable post-restart; first-hit p50 | Redis cold-start under `appendonly yes` (AOF replay) |
| 8 | **mixed-blob admission (1 KB / 100 KB / 5 MiB)** | Randomised mix of small / medium / large blobs into a fixed L1 budget. Working set is **2× L1**. | L1 hit rate; eviction count; p50 / p99 of `put` | Redis `SET` with the same mix; `ResultCache` for the small-blob slice only (it is not a blob store) |

Workload #8 is the SIEVE vs W-TinyLFU oracle. See next section.

### Working-set sizing convention

- `WS == 0.5 × L1` — fits comfortably in L1; drives "no admission
  pressure" baseline.
- `WS == 1.0 × L1` — at-capacity; SIEVE's recency bias starts to show.
- `WS == 2.0 × L1` — over-capacity; this is where W-TinyLFU's
  frequency admission would diverge from SIEVE if it is going to.
- `WS == 4.0 × L1 + L2 active` — drives L2 hit rate up; only used by
  workloads 2, 4, 7.

## SIEVE vs W-TinyLFU decision criterion

Per ADR 0006 §"L1 memory" + §"Open questions", the L1 ships with
SIEVE because RedDB already documents and ships SIEVE for page-cache
workloads. The criterion for migrating L1 to W-TinyLFU is:

> **Migrate trigger.** On the **mixed-blob admission** workload
> (#8) at `WS == 2.0 × L1`, if W-TinyLFU's hit rate beats SIEVE's
> hit rate by **more than 5 percentage points** under either
> persistence-off or `appendfsync everysec` Redis baselines, file a
> follow-up issue to swap the L1 admission policy. Below 5 pp the
> ADR-pinned default holds and SIEVE stays.

Why 5 pp:

- Below ~3 pp the comparison is inside the per-cell variance
  observed in the page-cache SIEVE bench history (see prior art in
  `docs/perf/wins.md` style sessions). Migrating on a noise-band
  delta would be churn.
- Between 3 and 5 pp the win is real but small enough that the
  added implementation surface (frequency sketch + admission
  filter) is hard to justify against the page-cache symmetry win
  of "one eviction policy, one bug surface".
- Above 5 pp the hit-rate delta dominates the implementation cost,
  especially because workload #4 (large blobs) is the place this
  matters most — a single 5 MiB miss costs orders of magnitude more
  than a 1 KB hit-rate point.

Reporting shape for the migration call:

```
[mixed-blob admission, WS=2.0×L1, persistence=no-persist]
SIEVE      hit-rate: TBD %   p50: TBD µs   p99: TBD µs
W-TinyLFU  hit-rate: TBD %   p50: TBD µs   p99: TBD µs
delta:     TBD pp     trigger fires at > 5 pp
```

The W-TinyLFU column is filled only if the implementation lands as
an opt-in feature flag on the bench harness; otherwise it is `n/a`
and the SIEVE row stands alone as the shipped policy's measurement.

## Result tables

All numbers `TBD` until the harness runs. Units are real; rows are
real; the placeholder is only the cell value.

### Workload 1 — hot-l1-hit

| backend | mode | ops/sec | p50 µs | p99 µs |
|---------|------|--------:|-------:|-------:|
| RedDB BlobCache (L1) | single-shot | TBD | TBD | TBD |
| RedDB ResultCache | single-shot | TBD | TBD | TBD |
| Redis 7.4 (no-persist) | single-shot | TBD | TBD | TBD |
| Redis 7.4 (no-persist) | pipelined | TBD | TBD | TBD |
| Redis 7.4 (aof-everysec) | single-shot | TBD | TBD | TBD |

### Workload 2 — cold-l2-miss

| backend | mode | ops/sec | p50 µs | p99 µs |
|---------|------|--------:|-------:|-------:|
| RedDB BlobCache (L1 cold, L2 hit) | single-shot | TBD | TBD | TBD |
| Redis 7.4 (aof-everysec, AOF replay warm) | single-shot | TBD | TBD | TBD |

### Workload 3 — cold-absent (synopsis effectiveness)

| backend | mode | ops/sec | p50 µs | p99 µs | l2-skip-rate |
|---------|------|--------:|-------:|-------:|-------------:|
| RedDB BlobCache (synopsis says absent) | single-shot | TBD | TBD | TBD | TBD % |
| RedDB ResultCache | single-shot | TBD | TBD | TBD | n/a |
| Redis 7.4 (no-persist) | single-shot | TBD | TBD | TBD | n/a |

### Workload 4 — large-blob-l2-hit (5 MiB)

| backend | mode | MB/sec | p50 ms | p99 ms |
|---------|------|-------:|-------:|-------:|
| RedDB BlobCache (L2 hit, 5 MiB) | single-shot | TBD | TBD | TBD |
| Redis 7.4 (no-persist) | single-shot | TBD | TBD | TBD |
| Redis 7.4 (aof-everysec) | single-shot | TBD | TBD | TBD |

### Workload 5 — namespace-flush

| backend | mode | flush-call p50 ms | flush-call p99 ms | reclaim-lag p50 ms |
|---------|------|------------------:|------------------:|-------------------:|
| RedDB BlobCache (generation bump) | foreground | TBD | TBD | TBD |
| Redis 7.4 (`FLUSHDB`) | foreground | TBD | TBD | n/a |
| Redis 7.4 (`SCAN`+`DEL` per prefix) | foreground | TBD | TBD | n/a |

### Workload 6 — dependency-invalidation

| backend | mode | invalidated count | p50 ms | p99 ms |
|---------|------|------------------:|-------:|-------:|
| RedDB BlobCache (dep-tag) | single-shot | TBD | TBD | TBD |
| RedDB ResultCache (`invalidate_dependent_caches`) | single-shot | TBD | TBD | TBD |
| Redis 7.4 (Lua tag-set sweep) | single-shot | TBD | TBD | TBD |

### Workload 7 — restart-warm-cache

| backend | persistence | open ms | first-hit p50 µs | post-restart entries reachable |
|---------|-------------|--------:|-----------------:|-------------------------------:|
| RedDB BlobCache (L2 on disk) | native blob-chain | TBD | TBD | TBD |
| Redis 7.4 | aof-everysec | TBD | TBD | TBD |

### Workload 8 — mixed-blob admission

| backend / policy | WS / L1 | hit-rate | evictions | p50 µs | p99 µs |
|------------------|--------:|---------:|----------:|-------:|-------:|
| RedDB BlobCache, SIEVE | 0.5 | TBD % | TBD | TBD | TBD |
| RedDB BlobCache, SIEVE | 1.0 | TBD % | TBD | TBD | TBD |
| RedDB BlobCache, SIEVE | 2.0 | TBD % | TBD | TBD | TBD |
| RedDB BlobCache, W-TinyLFU (if flagged) | 2.0 | TBD % | TBD | TBD | TBD |
| Redis 7.4 (allkeys-lru) | 2.0 | TBD % | TBD | TBD | TBD |

## Interpretation (placeholders ready to fill)

The interpretation section is laid out here so that, once the
numbers land, the report can be filled in without restructuring.

### Where Blob Cache is expected to win

- **Workload 1 (hot-l1-hit) vs Redis single-shot.** No socket RTT
  for the embedded `BlobCache::get` call path. Expected gap: large
  in single-shot, narrows under pipelined Redis.
- **Workload 3 (cold-absent).** Synopsis-skip should make
  RedDB faster than the Redis miss path, which still pays a
  loopback RTT.
- **Workload 7 (restart-warm-cache).** Native blob-chain reload
  vs AOF replay. Expected to be the headline durability win —
  measured against `appendonly everysec` Redis specifically because
  that is the closest comparable durability point.
- **Workload 6 (dependency-invalidation).** RedDB has a first-class
  dependency index. Redis simulates this with Lua + secondary set
  lookups; the per-invalidation cost should diverge as the
  dependency fanout grows.

### Where Redis is expected to win

- **Workload 1 pipelined.** Redis's pipelined GET amortises the
  loopback cost; the embedded path has no equivalent batching
  primitive at the same layer.
- **Workload 4 (5 MiB blobs) under no-persist.** Pure memory-to-
  memory throughput on Redis is the apex; the RedDB native blob-
  chain reassembly is the cost point. Compared properly against
  the `aof-everysec` Redis variant, the gap shrinks.

### Where neither is the right answer

- **Workload 8 (mixed-blob admission)** under `WS == 0.5 × L1`.
  Both backends should be at the noise floor. This row exists to
  prove the harness, not to crown a winner.

### Decision points the report should resolve

1. Does L1-hot performance justify routing the existing
   `ResultCache` callers through `BlobCache` per ADR 0006 §"Rollout"
   step 4? Trigger: if RedDB BlobCache hot-l1-hit p50 is within
   ±10 % of `ResultCache` hot-path p50, the rollout is safe.
2. Does the cold-absent (synopsis) row beat the Redis miss row?
   If yes, the ADR's negative-filter design is validated. If no,
   reopen the synopsis structure choice (Bloom vs Cuckoo) before
   public API design starts.
3. Does the SIEVE vs W-TinyLFU delta on workload 8 cross the 5 pp
   trigger? See the criterion above.
4. Does the restart-warm-cache row produce a defensible "cache
   survives restart" story for [`docs/perf/wins.md`](wins.md)? If
   yes, this becomes the headline differentiator vs Redis.

## Reproducing

The full repro recipe lives in
[`bench/blob-cache/README.md`](../../bench/blob-cache/README.md).
The short form is:

```bash
# 1. Bring up the pinned Redis baselines (both variants).
cd bench/blob-cache
./redis-up.sh         # follows redis-setup.md

# 2. Run the suite (all 8 scenarios, both Redis variants, RedDB).
make blob-cache-bench OFFICIAL_RUNS=10

# 3. Stats roll up into the same session-id discipline as #154.
make blob-cache-stats
```

The `Makefile` target name `blob-cache-bench` is the **proposed**
name for the follow-up slice that wires the harness in. This
scaffold does **not** add it to the existing top-level `Makefile`
(see "Cargo.toml / Makefile additions needed" in the slice notes
at the bottom of the doc).

## What this scaffold intentionally leaves out

The slice that produced this file is **scaffold + setup docs only**.
The follow-up slice that fills in the `TBD` cells must:

1. Add a benchmark crate (or `criterion` / `divan` harness target)
   that exercises `BlobCache` and `ResultCache` directly. Needs a
   workspace `Cargo.toml` dev-dep edit to register it — flagged,
   not applied here.
2. Add a Redis client dependency (e.g. `redis = "0.27"` or current
   stable) for the baseline harness. Same flagged-not-applied
   constraint.
3. Add a `Makefile` target `blob-cache-bench` that orchestrates
   the Redis docker bring-up, the suite, and the rollup.
4. Wire the session-id emission so the cited session line at the
   top of this doc gets a real id instead of the
   `sess-canonical-pending` sentinel.
5. Run the suite and fill in every `TBD` cell.
6. Publish the SIEVE vs W-TinyLFU delta and either close this
   item or file the W-TinyLFU migration follow-up per the trigger
   above.

Until that happens, this document is the **interface** the bench
output will fill in, and the scenarios in
[`bench/blob-cache/scenarios.md`](../../bench/blob-cache/scenarios.md)
are the **contract** the harness must satisfy.
