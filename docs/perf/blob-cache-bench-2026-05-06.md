# Blob Cache benchmark suite — Redis baseline report — 2026-05-11

Status: **Redis baseline complete for the shipped harness**. This file
keeps the workload definitions, baseline configuration, the
SIEVE → W-TinyLFU migration trigger, and the result tables produced by
the session below.

Tracking issue: #149 — *"Blob Cache: benchmark impact against result
cache and Redis"*.

Parent issue: #139.

ADR: [`0006-tiered-blob-cache.md`](../adr/0006-tiered-blob-cache.md).
Spike doc: [`blob-cache-l2-spike.md`](blob-cache-l2-spike.md).

Cited session id: `sess-2026-05-11-redis-baseline`.

Rollup:
[`bench/blob-cache/results/sess-2026-05-11-redis-baseline.md`](../../bench/blob-cache/results/sess-2026-05-11-redis-baseline.md).

## TL;DR (session `sess-2026-05-11-redis-baseline`)

- L1 hot hit central latency vs result-cache: **320 ns vs 300 ns** (BlobCache 7% slower on the hot fast path; within ADR 0006 §Rollout step 4 trigger of ±10%).
- L1 hot hit vs Redis (loopback): **320 ns RedDB vs 143.80 µs Redis GET**. Redis pipelined `MGET-32` reaches **26.86 K elements/sec**.
- Cold-absent (synopsis-effective): **0.378 µs RedDB vs 148.97 µs Redis MISS**. Synopsis short-circuit reported **100.0%** L2 skip-rate.
- L2 hit (5 MiB blob) throughput vs Redis GET (5 MiB) throughput:
  **206.47 MiB/s RedDB vs 1.4752 GiB/s Redis no-persist** and
  **1.1580 GiB/s Redis aof-everysec**.
- SIEVE vs W-TinyLFU hit-rate gap on the mixed-blob workload:
  **not computed** because W-TinyLFU is not implemented as a bench flag.
  Shipped-policy SIEVE at `WS == 2.0 × L1` reports **54.2%** hit-rate,
  **10,120** evictions, and **0.730 µs** central latency.

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
  `crates/reddb-server/src/storage/cache/blob/cache.rs` with the constants
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

- Working dir: `bench/blob-cache/`.
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
SIEVE      hit-rate: 54.2 %   central: 0.730 µs   upper CI: 0.747 µs
W-TinyLFU  hit-rate: n/a      central: n/a        upper CI: n/a
delta:     n/a        trigger fires at > 5 pp
```

The W-TinyLFU column is filled only if the implementation lands as
an opt-in feature flag on the bench harness; otherwise it is `n/a`
and the SIEVE row stands alone as the shipped policy's measurement.

## Result tables

Criterion reports confidence intervals rather than p50/p99 samples.
The central latency column below is the Criterion-reported mean; the
tail proxy is the upper confidence bound from the same run.

### Workload 1 — hot-l1-hit

| backend | mode | ops/sec | central µs | tail proxy µs |
|---------|------|--------:|-------:|-------:|
| RedDB BlobCache (L1) | single-shot | 3.12 M | 0.320 | 0.321 |
| RedDB ResultCache | single-shot | 3.33 M | 0.300 | 0.305 |
| Redis 7.4 (no-persist) | single-shot | 6.95 K | 143.80 | 147.42 |
| Redis 7.4 (no-persist) | pipelined `MGET-32` | 26.86 K elements/sec | 1191.6 per batch | 1206.9 per batch |
| Redis 7.4 (aof-everysec) | single-shot | 6.22 K | 160.68 | 171.67 |

### Workload 2 — cold-l2-miss

| backend | mode | ops/sec | central µs | tail proxy µs |
|---------|------|--------:|-------:|-------:|
| RedDB BlobCache (L1 cold, L2 hit) | single-shot | 19.82 K | 50.461 | 51.009 |
| Redis 7.4 (aof-everysec, AOF replay warm) | single-shot | 5.34 K | 187.14 | 202.40 |

### Workload 3 — cold-absent (synopsis effectiveness)

| backend | mode | ops/sec | central µs | tail proxy µs | l2-skip-rate |
|---------|------|--------:|-------:|-------:|-------------:|
| RedDB BlobCache (synopsis says absent) | single-shot | 2.65 M | 0.378 | 0.381 | 100.0% |
| RedDB ResultCache | single-shot | 6.21 M | 0.161 | 0.164 | n/a |
| Redis 7.4 (no-persist) | single-shot | 6.71 K | 148.97 | 152.59 | n/a |

### Workload 4 — large-blob-l2-hit (5 MiB)

| backend | mode | throughput | central ms | tail proxy ms |
|---------|------|-------:|-------:|-------:|
| RedDB BlobCache (L2 hit, 5 MiB) | single-shot | 206.47 MiB/s | 24.217 | 24.681 |
| Redis 7.4 (no-persist) | single-shot | 1.4752 GiB/s | 3.3099 | 3.7685 |
| Redis 7.4 (aof-everysec) | single-shot | 1.1580 GiB/s | 4.2166 | 5.4122 |

### Workload 5 — namespace-flush

| backend | mode | flush-call central ms | flush-call tail proxy ms | reclaim-lag central ms |
|---------|------|------------------:|------------------:|-------------------:|
| RedDB BlobCache (generation bump) | foreground | 0.000177 | 0.000186 | n/a |
| Redis 7.4 (`FLUSHDB`) | foreground | 1.0801 | 1.1148 | n/a |
| Redis 7.4 (`SCAN`+`DEL` per prefix) | foreground | 117.50 | 119.24 | n/a |

### Workload 6 — dependency-invalidation

| backend | mode | invalidated count | central ms | tail proxy ms |
|---------|------|------------------:|-------:|-------:|
| RedDB BlobCache (dep-tag) | single-shot | 250 | 0.2702 | 0.2724 |
| RedDB ResultCache (`invalidate_dependent_caches`) | single-shot | 250 expected | 0.0593 | 0.0614 |
| Redis 7.4 (Lua tag-set sweep) | single-shot | 250 expected | 0.5956 | 0.6184 |

### Workload 7 — restart-warm-cache

| backend | persistence | open ms | first-hit central µs | post-restart entries reachable |
|---------|-------------|--------:|-----------------:|-------------------------------:|
| RedDB BlobCache (L2 on disk) | native blob-chain | reopen+first-hit combined | 1160.2 | 127/128 |
| Redis 7.4 | aof-everysec | 265.684 | 1275.310 | 128/128 |

### Workload 8 — mixed-blob admission

| backend / policy | WS / L1 | hit-rate | evictions | central µs | tail proxy µs |
|------------------|--------:|---------:|----------:|-------:|-------:|
| RedDB BlobCache, SIEVE | 0.5 | 100.0% | 0 | 0.455 | 0.465 |
| RedDB BlobCache, SIEVE | 1.0 | 100.0% | 0 | 0.529 | 0.535 |
| RedDB BlobCache, SIEVE | 2.0 | 54.2% | 10,120 | 0.730 | 0.747 |
| RedDB BlobCache, W-TinyLFU (if flagged) | 2.0 | n/a | n/a | n/a | n/a |
| Redis 7.4 (allkeys-lru) | 2.0 | 100.0% | 0 | 124.59 | 127.15 |

## Interpretation

The interpretation below uses the cited session's central latency and
throughput values.

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
   step 4? Trigger: if RedDB BlobCache hot-l1-hit central latency is
   within ±10 % of `ResultCache` hot-path central latency, the rollout
   is safe.
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
# 1. Bring up the pinned Redis baselines (both variants) from repo root.
bench/blob-cache/redis-down.sh --wipe-aof
bench/blob-cache/redis-up.sh

# 2. Run the suite (all 8 reported scenarios, RedDB + Redis).
mkdir -p bench/blob-cache/results
REDIS_NO_PERSIST_ADDR=127.0.0.1:6379 \
REDIS_AOF_ADDR=127.0.0.1:6380 \
  cargo bench -p reddb-io-server --bench blob_cache_bench 'w[1-8]' -- --nocapture \
  2>&1 | tee bench/blob-cache/results/sess-<date>-redis-baseline.raw.log

# 3. Run workload-7 Redis AOF restart manually using redis-setup.md.
docker exec reddb-bench-redis-aof-everysec redis-cli -p 6379 BGREWRITEAOF
# Poll INFO persistence until aof_rewrite_in_progress is 0, restart the
# container against the same reddb-bench-redis-aof volume, poll PING, then
# issue GET for the known populated key set.
```

The rollup for the cited session is
[`bench/blob-cache/results/sess-2026-05-11-redis-baseline.md`](../../bench/blob-cache/results/sess-2026-05-11-redis-baseline.md).
The raw Criterion output from that run is in the adjacent `.raw.log`
files.

## Remaining scope

The W-TinyLFU row is intentionally `n/a` because the shipped harness has
no W-TinyLFU admission-policy flag. If that implementation lands later,
rerun workload 8 and compare against the 5 pp trigger above.
