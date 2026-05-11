# Blob Cache benchmark suite

This directory holds the benchmark suite that compares RedDB's
[`BlobCache`](../../crates/reddb-server/src/storage/cache/blob.rs)
against:

- the existing
  [`ResultCache`](../../crates/reddb-server/src/storage/cache/result.rs)
  (where the workload makes sense — `ResultCache` is a SQL result
  cache, not a blob store, so it only participates in the small-blob
  / hot-path scenarios), and
- a pinned local **Redis 7.4** baseline running in two persistence
  modes (`no-persist` and `appendonly everysec`).

The goal — per issue #149 — is to validate whether Blob Cache
produces real impact before any public API design lands.

The bench report lives at
[`docs/perf/blob-cache-bench-2026-05-06.md`](../../docs/perf/blob-cache-bench-2026-05-06.md).
The eight scenarios (with full per-scenario parameters) are
formalised in [`scenarios.md`](scenarios.md). The exact Redis
docker invocation lives in [`redis-setup.md`](redis-setup.md).

## Status

**Runnable harness.** The Criterion bench target lives at
`crates/reddb-server/benches/blob_cache_bench.rs`. Redis baseline rows
run when `REDIS_NO_PERSIST_ADDR` and `REDIS_AOF_ADDR` point at the
pinned containers from `redis-up.sh`.

## Layout

```
bench/blob-cache/
  README.md         # this file
  scenarios.md      # 8 workloads with parameters
  redis-setup.md    # pinned Redis docker invocation
  redis-up.sh       # starts both Redis variants
  redis-down.sh     # stops both Redis variants
  results/          # per-session rollups and raw logs
```

## Methodology — same lock as `make duel-official`

The Blob Cache suite reuses the methodology lock from issue #154
(`make duel-official` in
[`rdb-benchmark`](https://github.com/reddb-io/rdb-benchmark)):

- 10 runs per cell (`OFFICIAL_RUNS = 10`).
- First 2 iterations of each run discarded as warmup.
- One **session id** per full sweep, captured into the report's
  `Cited session` slot. Re-running on a different host or with
  different config produces a new session id; numbers do not get
  edited in place.
- `--release` builds, default workspace features, no fsync tricks
  on the RedDB side beyond what the scenario explicitly pins.

## Running the suite

```bash
# 0. From repo root.
chmod +x bench/blob-cache/redis-up.sh bench/blob-cache/redis-down.sh

# 1. Bring up both pinned Redis baselines.
bench/blob-cache/redis-down.sh --wipe-aof
bench/blob-cache/redis-up.sh   # see redis-setup.md for what this runs

# 2. Run the full suite.
mkdir -p bench/blob-cache/results
REDIS_NO_PERSIST_ADDR=127.0.0.1:6379 \
REDIS_AOF_ADDR=127.0.0.1:6380 \
  cargo bench -p reddb-server --bench blob_cache_bench 'w[1-8]' -- --nocapture \
  2>&1 | tee bench/blob-cache/results/sess-<date>-redis-baseline.raw.log

# 3. Run a single scenario (mirrors `make mini-duel`).
cargo bench -p reddb-server --bench blob_cache_bench w1-hot-l1-hit -- --nocapture

# 4. Write the rollup next to the raw log and cite it from the report.
$EDITOR bench/blob-cache/results/sess-<date>-redis-baseline.md

# 5. Tear down the Redis baselines.
bench/blob-cache/redis-down.sh
```

## Per-scenario gotchas

Each scenario has parameters (payload size, op count, working-set
size relative to L1, prefetch on/off, etc) that are pinned in
[`scenarios.md`](scenarios.md). When running a single scenario,
filter the Criterion target rather than editing the scenarios doc:

```bash
# Override op count for a quick smoke run of the large-blob scenario.
cargo bench -p reddb-server --bench blob_cache_bench w4-large-blob-l2-hit -- --nocapture
```

Editing `scenarios.md` re-pins the contract for everyone — only
do that with intent.

## Interpreting the output

See the **Interpretation** section of
[`docs/perf/blob-cache-bench-2026-05-06.md`](../../docs/perf/blob-cache-bench-2026-05-06.md)
for the decision points the report is set up to resolve. The
short version:

1. **Hot-l1-hit vs ResultCache** — does it justify the ADR 0006
   §"Rollout" step 4 of routing `ResultCache` callers through
   `BlobCache`?
2. **Cold-absent (synopsis) vs Redis miss** — does the synopsis
   actually skip L2 reads, and does it beat a loopback-RTT miss?
3. **Workload 8 (mixed-blob admission)** — does W-TinyLFU beat
   SIEVE by **> 5 pp**? If yes, file the migration. If no, ship
   SIEVE.
4. **Restart-warm-cache** — is the "cache survives restart" story
   strong enough to land in
   [`docs/perf/wins.md`](../../docs/perf/wins.md)?

## Why a separate suite (not just `rdb-benchmark`)?

`rdb-benchmark` is the cross-engine duel for SQL / KV scenarios
(typed insert, bulk update, aggregate group, etc). Blob Cache is
not a SQL workload and the comparison universe is different
(Redis, not Postgres / Mongo). Co-locating the suite under
`bench/` keeps it discoverable next to the other bench artifacts
(`bench/cold-start-baseline.md` etc.) without distorting
`rdb-benchmark`'s shape.

The methodology lock and session-id discipline are shared on
purpose — when both reports cite the same host and the same
session-id format, cross-referencing across the two suites stays
trivial.

## Current cited session

The report currently cites
[`results/sess-2026-05-11-redis-baseline.md`](results/sess-2026-05-11-redis-baseline.md).
That rollup records the exact command sequence, Redis image digest,
host metadata, and raw log paths for the filled result tables.
