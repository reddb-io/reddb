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

The bench report skeleton lives at
[`docs/perf/blob-cache-bench-2026-05-06.md`](../../docs/perf/blob-cache-bench-2026-05-06.md).
The eight scenarios (with full per-scenario parameters) are
formalised in [`scenarios.md`](scenarios.md). The exact Redis
docker invocation lives in [`redis-setup.md`](redis-setup.md).

## Status

**Scaffold only.** This README, the scenarios doc, and the Redis
setup doc are the contract the bench harness must satisfy. The
harness itself (Rust crate / criterion targets / make targets)
is the next slice — see "Follow-up slice" at the bottom of this
file.

## Layout

```
bench/blob-cache/
  README.md         # this file
  scenarios.md      # 8 workloads with parameters
  redis-setup.md    # pinned redis docker invocation
```

The follow-up slice will add (proposed names; not yet present):

```
bench/blob-cache/
  redis-up.sh       # wraps the docker invocation in redis-setup.md
  redis-down.sh     # tears down both persistence variants
  results/          # per-session rollups (session-id discipline)
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

## Running the suite (proposed shape)

> **Note.** Commands below describe the **proposed** harness shape
> for the follow-up slice. Today, only the scenario specs and the
> Redis baseline pin are present. Running them now requires
> implementing the harness first.

```bash
# 0. From repo root.
cd bench/blob-cache

# 1. Bring up both pinned Redis baselines.
./redis-up.sh                 # see redis-setup.md for what this runs

# 2. Run the full suite.
make blob-cache-bench         # proposed top-level make target

# 3. Run a single scenario (mirrors `make mini-duel`).
make blob-cache-bench SCENARIOS=hot-l1-hit RUNS=3

# 4. Roll up results into the report's TBD cells.
make blob-cache-stats SESSION=sess-<timestamp>-<pid>

# 5. Tear down the Redis baselines.
./redis-down.sh
```

## Per-scenario gotchas

Each scenario has parameters (payload size, op count, working-set
size relative to L1, prefetch on/off, etc) that are pinned in
[`scenarios.md`](scenarios.md). When running a single scenario,
override its params with `make`-style flags rather than editing
the scenarios doc:

```bash
# Override op count for a quick smoke run of the large-blob scenario.
make blob-cache-bench SCENARIOS=large-blob-l2-hit OPS=200 RUNS=3
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

## Follow-up slice

The implementation slice that finishes this work needs to:

- Add a benchmark crate (or criterion / divan target) that
  exercises `BlobCache` and `ResultCache`. **Requires editing
  `Cargo.toml` to register the dev-dep + bench target** —
  intentionally not done in this scaffold slice.
- Add the Redis client dep for the baseline harness. **Same
  Cargo.toml constraint.**
- Add `redis-up.sh` / `redis-down.sh` matching
  [`redis-setup.md`](redis-setup.md).
- Add the `blob-cache-bench` and `blob-cache-stats` targets to
  the top-level `Makefile`. **Requires editing the existing
  `Makefile`** — intentionally not done in this scaffold slice.
- Run the suite and fill in the `TBD` cells in the report.
