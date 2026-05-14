# Metrics: aggregation, grouping, and simple arithmetic [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Extend the PromQL adapter and metrics executor to support common dashboard aggregations and grouping. Queries such as `sum by (service) (rate(http_requests_total[5m]))` should compile to native metrics plans and return Prometheus-shaped results.

## Acceptance criteria

- [x] `sum`, `avg`, `min`, `max`, and `count` work over instant and range contexts covered by the v0 matrix.
- [x] `by (...)` and `without (...)` grouping produce correct label sets.
- [x] Simple arithmetic supported by the v0 matrix works over compatible vectors/scalars.
- [x] Unsupported vector matching or advanced operators fail clearly.
- [x] Golden tests cover representative Grafana dashboard queries.

## Blocked by

- issues/486-prometheus-query-range-grafana-panels.md
