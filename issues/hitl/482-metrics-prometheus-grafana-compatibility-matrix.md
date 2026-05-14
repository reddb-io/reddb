# Metrics: Prometheus/Grafana compatibility matrix [HITL]

Labels: research, needs-triage

## HITL instruction

This issue requires human review before implementation. The compatibility line determines what RedDB promises to Prometheus, Grafana, and migration customers in v0.

## Parent

issues/prd/reddb-metrics-backend-v0.md

## What to build

Define the v0 compatibility matrix for RedDB as a Prometheus-compatible metrics backend. Study pinned Prometheus and Grafana upstreams under `.study/upstream/`, then document the exact v0 contract: remote-write fixtures, Grafana datasource expectations, query endpoints, supported PromQL subset, histogram behavior, unsupported features, and smoke-dashboard acceptance criteria.

## Acceptance criteria

- [ ] Prometheus and Grafana upstreams are cloned or referenced under ignored `.study/upstream/` paths with pinned tags/commits.
- [ ] A compatibility matrix documents supported endpoints, request/response shapes, PromQL functions/operators, histogram support, and explicit out-of-scope behavior.
- [ ] Real remote-write and Grafana query fixtures are identified for downstream implementation tests.
- [ ] The matrix states how unsupported PromQL must fail and which Grafana dashboard patterns are expected to work in v0.
- [ ] The matrix is reviewed by a human before implementation issues depend on it.

## Blocked by

None - can start immediately, but blocked on design review before implementation.
