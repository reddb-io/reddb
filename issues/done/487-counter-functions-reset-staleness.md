# Metrics: counter functions with reset and staleness semantics [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Support the first operational PromQL functions over counters: `rate`, `irate`, and `increase`. The implementation must handle process restarts/counter resets, lookback windows, and practical staleness behavior so common request-rate and error-rate dashboards work.

## Acceptance criteria

- [x] `rate(counter[window])`, `irate(counter[window])`, and `increase(counter[window])` work through `/api/v1/query` and `/api/v1/query_range`.
- [x] Counter reset fixtures produce sane rates/increases after a reset.
- [x] Lookback/staleness behavior is documented and covered by tests.
- [x] Grafana-shaped query_range fixtures using counter functions return stable matrix outputs.
- [x] Unsupported function shapes fail clearly.

## Blocked by

- issues/486-prometheus-query-range-grafana-panels.md
