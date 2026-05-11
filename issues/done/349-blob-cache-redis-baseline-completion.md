# Blob Cache Redis baseline completion [AFK]

GitHub: local follow-up from reddb-io/reddb#339 / #149

Labels: enhancement, ready-for-agent

GitHub issue number: #349

## Parent

#339 (https://github.com/reddb-io/reddb/issues/339)

## What to build

Fill the remaining Redis and hit-rate cells in
`docs/perf/blob-cache-bench-2026-05-06.md` using the existing
`crates/reddb-server/benches/blob_cache_bench.rs` harness and pinned
`bench/blob-cache/` Redis 7.4 setup.

Covers: remaining Redis and hit-rate benchmark acceptance from #149

## Acceptance criteria

- [x] Redis no-persist and AOF-everysec baseline cells are populated for the workloads where Redis is applicable.
- [x] SIEVE hit-rate cells are populated for mixed-blob admission.
- [x] The cited session id is replaced with the session that produced the final tables.
- [x] The benchmark doc no longer uses `deferred` or `TBD` for cells that the harness can measure locally.
- [x] Public instructions record the exact command sequence, Redis setup, host/session metadata, and result rollup path.

## Blocked by

None.

## Done

- Session: `sess-2026-05-11-redis-baseline`.
- Rollup: `bench/blob-cache/results/sess-2026-05-11-redis-baseline.md`.
- Raw logs: `bench/blob-cache/results/sess-2026-05-11-redis-baseline*.raw.log`.
- Notes: W-TinyLFU remains `n/a` because the shipped harness has no
  W-TinyLFU admission-policy flag; SIEVE shipped-policy hit-rate cells
  are populated.
