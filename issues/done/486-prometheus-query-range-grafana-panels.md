# Metrics: Prometheus `/api/v1/query_range` for Grafana panels [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Expose a Prometheus-compatible range query endpoint for Grafana time-series panels. A Grafana Prometheus datasource can request `start`, `end`, and `step` over metrics ingested into RedDB and receive matrix responses with practical step alignment.

## Acceptance criteria

- [x] `/api/v1/query_range` accepts `query`, `start`, `end`, and `step`.
- [x] Simple selectors return Prometheus-shaped matrix responses with timestamp/value pairs.
- [x] Step alignment matches the compatibility matrix and is covered by golden tests.
- [x] A minimal Grafana datasource smoke or equivalent HTTP fixture proves a panel can read from RedDB.
- [x] Invalid ranges and unsupported expressions return clear Prometheus-shaped errors.

## Blocked by

- issues/485-prometheus-query-instant-selectors.md
