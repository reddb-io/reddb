# KV — Observability + CI benchmarks (stats, p50/p99 vs Redis baseline) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/253

Labels: enhancement

GitHub issue number: #253

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#238

## What to build

Operational tail: surface per-verb stats counters from the runtime (`puts`, `gets`, `deletes`, `incrs`, `cas_success`, `cas_conflict`, `watch_streams_active`, `watch_events_emitted`, `watch_drops`) and pin a CI benchmark suite that compares RedDB to a Redis baseline on the same workload — `PUT / GET / INCR` p50 + p99, plus `WATCH` event-delivery lag. Regressions block merge.

## Acceptance criteria

- [ ] `KvAtomicOps` increments per-verb counters atomically on every operation. `KvWatchStream` exports stream-count + drop-count gauges.
- [ ] `red doctor` and `/stats` HTTP endpoint expose the new counters in the same shape as existing engine counters.
- [ ] CI benchmark suite under `bench/` runs `PUT / GET / INCR` and reports p50, p99, throughput. Same workload runs against a local `redis-server` for baseline comparison.
- [ ] CI gate: a regression > 20% on any p99 vs the previous tagged release blocks merge. Threshold is configurable.
- [ ] WATCH benchmark: writer commits 100k events, subscriber reads, measures delivery lag and drop count. Lag p99 < 10 ms target on 4-vCPU.
- [ ] Benchmark results are checked into `bench/results/` per release and published in the docs.
- [ ] `monitoring.md` doc updated with the new stats keys + scrape recipes for Prometheus.

## Blocked by

- #241
- #242
- #243
- #245
