# Metrics: Grafana compatibility smoke and migration docs [AFK]

Labels: test, docs, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Create the capstone compatibility check for RedDB Metrics Backend v0. A Grafana Prometheus datasource should point at RedDB, render representative panels from remote-write-ingested data, and the docs should explain how to migrate from Prometheus plus what PromQL/features are supported or unsupported.

## Acceptance criteria

- [ ] A documented smoke procedure or CI job starts RedDB, ingests fixture metrics, configures Grafana's Prometheus datasource, and renders representative panels.
- [ ] Smoke coverage includes selectors, range queries, counter functions, aggregations, classic histogram quantiles, cardinality rejection visibility, tenant isolation, and rollup-backed long ranges.
- [ ] Migration docs explain remote-write setup through Prometheus, Grafana Alloy, or OpenTelemetry Collector.
- [ ] Compatibility docs list supported PromQL/features and explicit v0 limitations.
- [ ] Failures point to the responsible implementation slice.

## Blocked by

- issues/486-prometheus-query-range-grafana-panels.md
- issues/487-counter-functions-reset-staleness.md
- issues/488-promql-aggregation-grouping-arithmetic.md
- issues/489-classic-histogram-ingest-quantile.md
- issues/490-cardinality-budgets-partial-accept-quarantine.md
- issues/491-raw-ttl-rollups-tier-selection.md
- issues/492-metrics-tenant-rls-isolation.md
