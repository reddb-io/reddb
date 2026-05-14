# Metrics: classic histogram ingest and `histogram_quantile` [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Support classic Prometheus histograms as a first-class metrics workload. RedDB should ingest `_bucket`, `_sum`, and `_count` series through remote-write and answer `histogram_quantile` queries for p50, p95, and p99 latency dashboards.

## Acceptance criteria

- [ ] Remote-write fixtures with classic histogram series ingest successfully.
- [ ] Histogram bucket, sum, and count series preserve labels and tenant/namespace identity.
- [ ] `histogram_quantile()` returns expected values for p50, p95, and p99 fixtures.
- [ ] Query API responses remain Prometheus/Grafana-compatible.
- [ ] Prometheus native histograms remain explicitly unsupported unless covered by the compatibility matrix.

## Blocked by

- issues/486-prometheus-query-range-grafana-panels.md
