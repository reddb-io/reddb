# Metrics: `CREATE METRICS` minimal collection contract [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Add the minimal RedDB-native metrics collection contract. A user can declare a metrics collection, inspect it through catalog/introspection surfaces, and persist enough metadata for tenant/namespace identity, raw retention, and future rollup/cardinality policies. This slice establishes the product model without implementing Prometheus ingestion yet.

## Acceptance criteria

- [x] `CREATE METRICS <name>` or the approved v0 spelling creates a cataloged metrics collection.
- [x] Metrics collections appear in RedDB introspection with a distinct metrics model/kind.
- [x] Collection metadata persists across reopen and records at least raw retention plus tenant/namespace-aware identity configuration.
- [x] DDL docs/conformance cover create, duplicate create, invalid options, show/list, drop/truncate behavior as applicable.
- [x] Existing time-series, table, queue, and red-schema behavior is unchanged.

## Verification

- `rtk cargo fmt --check`
- `rtk cargo test --test e2e_metrics_collection_contract -- --nocapture`
- `rtk make check`

## Blocked by

None - compatibility matrix resolved in
issues/done/482-metrics-prometheus-grafana-compatibility-matrix.md and
docs/operations/metrics-compatibility.md.
