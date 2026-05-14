# Metrics: raw TTL, rollups, and automatic tier selection [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Add layered retention to metrics collections: raw samples expire on their raw TTL, rollup tiers retain coarser data for longer windows, and query_range automatically reads the cheapest tier that preserves the requested resolution.

## Acceptance criteria

- [x] Metrics collections can declare raw TTL plus at least one rollup tier.
- [x] Rollups are materialized from ingested samples and survive after raw samples expire.
- [x] Retention removes expired raw data without removing still-valid rollup data.
- [x] `query_range` selects raw or rollup data based on requested range/step according to documented rules.
- [x] Tests cover raw-only, rollup-selected, and expired-raw query behavior.

## Blocked by

- issues/486-prometheus-query-range-grafana-panels.md
