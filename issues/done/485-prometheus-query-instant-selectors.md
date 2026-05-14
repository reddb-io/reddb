# Metrics: Prometheus `/api/v1/query` for instant selectors [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Expose a Prometheus-compatible instant query endpoint for simple metric selectors over RedDB metrics. Grafana and curl clients can query a metric by name and label matchers and receive Prometheus-shaped vector responses from data ingested through remote-write.

## Acceptance criteria

- [x] `/api/v1/query` accepts simple selectors such as `metric_name` and `metric_name{label="value"}`.
- [x] Equality and negative label matchers supported by the v0 matrix return correct series.
- [x] Responses follow Prometheus `status/data/resultType/result` envelope shape for vectors.
- [x] Unsupported PromQL returns a clear Prometheus-shaped error instead of panicking or returning wrong data.
- [x] Tests ingest fixture samples via remote-write and query them through the HTTP API.

## Blocked by

- issues/484-remote-write-ingest-counters-gauges.md
