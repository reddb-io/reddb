# Metrics: phase 2 alert events over queues [HITL]

Labels: design, needs-triage

## HITL instruction

This issue requires human design review. It decides how far RedDB should go toward replacing Prometheus rules and Alertmanager after the metrics backend v0 is proven.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Design the phase 2 alerting model where RedDB evaluates alert rules against the metrics engine and emits alert state-change events into RedDB queues. The design should cover pending/firing/resolved state, retry/fanout/DLQ, notification workers, audit/history, and the boundary with Grafana Alerting.

## Acceptance criteria

- [ ] A design note explains whether RedDB evaluates rules itself, delegates to Grafana Alerting, or supports both.
- [ ] Alert state-change event shape is defined for queue delivery.
- [ ] Queue retry, fanout, DLQ, and audit behavior are specified.
- [ ] Interaction with tenant/RLS, namespaces, and notification credentials is specified.
- [ ] Follow-up AFK implementation issues can be created from the design.

## Blocked by

- issues/493-grafana-compatibility-smoke-docs.md
