# Metrics: remote-write ingest for counters and gauges [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Accept Prometheus `remote_write` requests for counter and gauge samples, decode batches, map each series into the RedDB metrics collection model, persist samples durably, and expose ingestion counters for accepted/rejected samples. This is the first end-to-end write path from a Prometheus-compatible collector into RedDB metrics storage.

## Acceptance criteria

- [x] A Prometheus-compatible remote-write endpoint accepts fixture payloads for counters and gauges.
- [x] Samples are stored through a metrics-native batch path, not by generating one SQL row per sample.
- [x] Accepted sample/series counts and rejected sample/series counts are observable through RedDB metrics/admin surfaces.
- [x] WAL/reopen tests prove ingested samples survive restart.
- [x] Invalid payloads fail clearly without corrupting already accepted data.

## Blocked by

- issues/483-create-metrics-minimal-collection-contract.md
