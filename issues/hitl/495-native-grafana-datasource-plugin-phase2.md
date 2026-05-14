# Metrics: phase 2 native Grafana datasource plugin [HITL]

Labels: design, needs-triage

## HITL instruction

This issue requires human product/design review. It decides whether a RedDB-native Grafana plugin is worth building after Prometheus datasource compatibility works.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Design a phase 2 native Grafana datasource plugin for RedDB-only capabilities: SQL over metrics, correlation with events, cohorts, document joins, tenant-aware exploration, rollup explainability, and richer statistics than PromQL exposes.

## Acceptance criteria

- [ ] A design note states which RedDB-only workflows justify a native plugin over the built-in Prometheus datasource.
- [ ] The plugin's query modes are specified: PromQL passthrough, SQL, metrics explorer, event correlation, or another approved set.
- [ ] Auth, tenant/RLS behavior, and datasource configuration are specified.
- [ ] The design identifies what must remain available through the Prometheus-compatible datasource.
- [ ] Follow-up AFK implementation issues can be created from the design.

## Blocked by

- issues/493-grafana-compatibility-smoke-docs.md
