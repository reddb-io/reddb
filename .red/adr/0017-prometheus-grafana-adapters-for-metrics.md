# ADR 0017 - Prometheus and Grafana adapters for RedDB Metrics

**Status:** Proposed
**Date:** 2026-05-14
**Related:** ADR 0010 (wire adapters translate, never duplicate), PRD: RedDB Metrics Backend v0

## Context

RedDB already exposes its own operator telemetry at `GET /metrics` in
Prometheus/OpenMetrics text format. RedDB also has native time-series storage,
retention, hypertables, continuous aggregates, queues, and events.

A customer-facing metrics backend is a different product surface. The goal is to
let customers remove Prometheus as their metrics backend while keeping Grafana
dashboards and common Prometheus-compatible collection pipelines. That requires
Prometheus `remote_write` ingestion and enough of the Prometheus HTTP query API
for Grafana.

The architectural risk is letting Prometheus concepts leak into the core engine:
PromQL as the engine language, Prometheus TSDB internals as the storage model,
or Grafana-specific response shapes as native query results. RedDB already has
an accepted adapter rule for wire compatibility: translate at the adapter
boundary and keep the engine's native surface canonical. Metrics compatibility
should follow the same rule.

## Decision

Prometheus and Grafana compatibility live in adapters. The core product is a
RedDB-native metrics engine.

Concretely:

- The engine owns native concepts such as metric collections, tenant,
  namespace, metric kind, normalized labels, series id, samples, histograms,
  rollup policy, retention policy, cardinality budget, and metrics logical
  plans.
- The Prometheus adapter owns `remote_write`, Prometheus HTTP API response
  envelopes, supported PromQL parsing, Prometheus error/status shapes, and
  Grafana datasource compatibility.
- The Grafana v0 connector is Grafana's built-in Prometheus datasource pointed
  at RedDB. A custom Grafana plugin is not part of the first milestone.
- Supported PromQL compiles into a native metrics logical plan. The adapter does
  not generate SQL strings, and the engine does not treat PromQL as its
  canonical query language.
- Classic Prometheus histograms are supported in v0 because common SRE
  dashboards depend on `histogram_quantile`. Prometheus native histograms are a
  follow-up unless customer fixtures require them.
- Alerting rules and Alertmanager replacement are follow-up work. If RedDB later
  evaluates alert rules, alert state-change events should use RedDB queues for
  retry, fanout, and DLQ.

## Consequences

- RedDB can satisfy practical Prometheus/Grafana compatibility without becoming
  a Prometheus internals clone.
- Grafana dashboard migration can start with a datasource URL change instead of
  a custom plugin rollout.
- The adapter must carry a clear compatibility matrix. Unsupported PromQL must
  fail explicitly.
- Metrics engine tests can focus on RedDB-native semantics: series identity,
  tenancy/RLS, retention, rollups, histograms, cardinality budgets, and query
  plans.
- Prometheus adapter tests must include real remote-write fixtures and
  Grafana-shaped query API responses.
- Future RedDB-only features such as SQL over metrics, correlation with events,
  cohorts, document joins, and tenant-aware analytics can reuse the same core
  metrics engine without changing the compatibility adapter.

## Considered Alternatives

**Implement Prometheus inside the engine.** Reuse Prometheus naming, TSDB block
layout, and PromQL as the core model.

Rejected because it imports Prometheus limitations and vocabulary into RedDB's
domain model. It also conflicts with the existing adapter discipline: external
compatibility surfaces should translate into RedDB-native concepts.

**Expose only SQL over `CREATE TIMESERIES`.** Tell Grafana users to migrate
dashboards to SQL or a custom plugin.

Rejected because the immediate customer value is replacing Prometheus without
rewriting dashboards and collection pipelines. SQL is a differentiator, not the
first compatibility bridge.

**Build a native Grafana plugin first.** Create a RedDB datasource before
supporting the Prometheus datasource shape.

Rejected for v0 because Grafana already has a mature Prometheus datasource.
Using it first reduces rollout risk and validates the metrics backend before a
plugin adds RedDB-only capabilities.

**Build a scraper first.** Replace both Prometheus collection and storage in one
step.

Rejected for v0 because scraping requires service discovery, relabeling, auth,
TLS, target health, retry behavior, and operational UX. `remote_write` lets
Grafana Alloy, OpenTelemetry Collector, or an existing Prometheus deployment
feed RedDB while RedDB proves the backend and query path.

## Follow-ups

- Create a Prometheus/Grafana compatibility matrix from pinned upstream clones
  under `.study/upstream/`.
- Define the `CREATE METRICS` DDL and catalog metadata.
- Define the native metrics logical plan and PromQL subset.
- Define cardinality budget policies and quarantine behavior.
- Define Grafana smoke-dashboard acceptance tests.
- Design phase 2 alerting: rule evaluation plus alert events emitted to queues.
