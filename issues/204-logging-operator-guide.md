# Logging: docs/operations/logging.md operator guide [AFK]

GitHub: reddb-io/reddb#204
Parent: #201

Additive new file `docs/operations/logging.md`. PG-style operator-facing guide covering:

1. Three telemetry channels (operator-grade / slow query / developer signal)
2. Sinks (audit log, red-slow.log, red.log + stderr) with location/rotation/retention/format
3. Config knobs table (RUST_LOG, slow_query_threshold_ms, sample_pct, etc)
4. 12 OperatorEvent variants listed with one-liner descriptions
5. Why three channels (rationale — per no-standalone-ADR rule, lives here)
6. PostgreSQL comparison table (logging_collector ↔ tracing_appender, log_min_duration_statement ↔ slow query log)
7. Cross-links to backup-restore + dashboards docs + ADR 0006/0008

## Acceptance Criteria

- [ ] docs/operations/logging.md exists with 7 sections.
- [ ] Each sink documented with location + rotation + retention + format.
- [ ] All 12 OperatorEvent variants listed.
- [ ] PG comparison table.
- [ ] Cross-links resolve.
- [ ] Word count 1500-2500.
