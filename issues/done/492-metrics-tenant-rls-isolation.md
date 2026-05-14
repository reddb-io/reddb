# Metrics: tenant/RLS isolation for ingest and query [AFK]

Labels: security, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Make tenant and namespace part of metrics series identity and enforce that boundary on ingest and query. Tenant should come from auth/header context, not from an ordinary Prometheus label. Grafana and remote-write clients should only see or mutate metrics admitted by their effective policy/RLS scope.

## Acceptance criteria

- [x] Ingestion resolves tenant/namespace from request/auth context and stores it as part of series identity.
- [x] Queries apply tenant/RLS filters before returning any series.
- [x] A caller cannot read another tenant's series by guessing metric names or labels.
- [x] Audit or equivalent observability records metrics ingest/query activity at tenant/namespace granularity.
- [x] Tests cover same-label series in different tenants and denied cross-tenant query attempts.

## Blocked by

- issues/485-prometheus-query-instant-selectors.md
