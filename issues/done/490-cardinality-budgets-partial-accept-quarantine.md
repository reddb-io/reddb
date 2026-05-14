# Metrics: cardinality budgets with partial accept and quarantine [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Enforce cardinality budgets during metrics ingestion. When a remote-write batch introduces too many new series for a tenant, namespace, metric, or configured budget, RedDB should accept valid in-budget samples, reject or quarantine over-budget series, and expose enough diagnostics for operators to understand what happened.

## Acceptance criteria

- [x] Series admission enforces configured budgets per tenant/namespace/metric according to the v0 contract.
- [x] Remote-write batches can partially accept valid samples while rejecting or quarantining over-budget series.
- [x] Rejections are observable through counters/admin output with reason and top offending labels/metrics where safe.
- [x] RedDB never silently drops labels to reduce cardinality.
- [x] Tests cover in-budget admission, over-budget rejection, partial batch behavior, and reopen stability of budget metadata.

## Blocked by

- issues/484-remote-write-ingest-counters-gauges.md
