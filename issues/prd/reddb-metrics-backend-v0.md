# PRD: RedDB Metrics Backend v0

Labels: prd, needs-triage

## Problem Statement

RedDB already has native time-series storage, retention, downsampling, continuous
aggregates, queues, events, and a Prometheus-format `/metrics` endpoint for its
own operator telemetry. That is not enough for a customer who wants to remove a
Prometheus server from their infrastructure while keeping Grafana dashboards and
the existing metrics ecosystem.

The customer need is concrete: RedDB should become the metrics backend. Existing
collectors should be able to write metrics to RedDB, Grafana should be able to
query RedDB as if it were a Prometheus datasource, and RedDB should store,
retain, roll up, and query those metrics using RedDB-native engine capabilities.

If RedDB only exposes SQL over generic time-series data, the customer must
rewrite dashboards and ingestion pipelines. If RedDB clones Prometheus internals
inside the engine, the product inherits Prometheus limitations and violates the
adapter boundary already established for wire compatibility. The goal is
external Prometheus/Grafana compatibility with a richer RedDB-native metrics
engine underneath.

## Solution

Ship a RedDB Metrics Backend v0 with three layers:

1. **RedDB Metrics Engine.** A native metrics model built on the existing
   time-series, hypertable, retention, chunking, batch-write, and continuous
   aggregate infrastructure. It owns metric series identity, tenant/namespace
   isolation, label normalization, TTL, rollups, cardinality budgets, histograms,
   and metrics-specific query semantics.
2. **Prometheus adapter.** A compatibility boundary that accepts Prometheus
   `remote_write` and exposes enough of the Prometheus HTTP query API for
   Grafana dashboards. Prometheus names, wire formats, and PromQL translation
   stay in the adapter.
3. **Grafana connector.** The first Grafana connector is not a custom plugin.
   Grafana uses its built-in Prometheus datasource pointed at RedDB. A native
   RedDB Grafana plugin can follow later for SQL, events, cohorts, joins, and
   RedDB-only analytics.

The MVP should let an operator remove Prometheus as the central metrics server
for dashboard reads while keeping collectors and Grafana mostly unchanged:

- Collectors send Prometheus `remote_write` to RedDB, directly or through
  Grafana Alloy / OpenTelemetry Collector.
- Grafana queries RedDB through `/api/v1/query` and `/api/v1/query_range`.
- Common SRE dashboards render using a documented PromQL subset.
- Raw samples and rollups obey RedDB retention policies.
- High-cardinality label explosions are rejected or quarantined explicitly
  instead of being silently dropped.

Scraping, recording rules, Alertmanager replacement, and a native Grafana plugin
are follow-up phases.

## User Stories

1. As an SRE, I want Grafana to use RedDB through the built-in Prometheus
   datasource, so that I can migrate dashboards without installing a custom
   Grafana plugin.
2. As an SRE, I want Prometheus-compatible `/api/v1/query`, so that instant
   dashboard panels can query RedDB.
3. As an SRE, I want Prometheus-compatible `/api/v1/query_range`, so that
   Grafana time-range panels render from RedDB.
4. As an SRE, I want common PromQL selectors like
   `http_requests_total{service="api"}`, so that existing dashboards keep their
   basic filters.
5. As an SRE, I want `rate`, `irate`, and `increase` over counters, so that
   request-rate and error-rate panels work.
6. As an SRE, I want `sum`, `avg`, `min`, `max`, and `count` with `by (...)`
   and `without (...)`, so that service, route, tenant, and instance rollups
   work.
7. As an SRE, I want classic Prometheus histograms to work with
   `histogram_quantile`, so that p50, p95, and p99 latency dashboards survive
   the migration.
8. As an SRE, I want query-range step alignment to match practical Grafana
   expectations, so that charts do not shift or duplicate points after moving
   to RedDB.
9. As an SRE, I want counter resets handled inside `rate` and `increase`, so
   that process restarts do not corrupt dashboards.
10. As an SRE, I want stale series to disappear after a documented lookback
    window, so that dead targets do not appear alive forever.
11. As an operator, I want Prometheus `remote_write` ingestion, so that existing
    scrape agents can write into RedDB.
12. As an operator, I want the ingestion path to batch samples natively, so that
    RedDB does not write one row per sample through SQL.
13. As an operator, I want RedDB to expose ingestion success, rejection, lag,
    and backpressure metrics, so that I can operate the metrics backend itself.
14. As an operator, I want raw-sample TTL plus longer-lived rollups, so that
    recent investigations have detail and long-range dashboards stay cheap.
15. As an operator, I want rollup selection to be automatic for long queries,
    so that a 90-day dashboard does not scan raw 15-second samples.
16. As an operator, I want retention policies per tenant or namespace, so that
    production, staging, and customer environments can have different costs.
17. As an operator, I want cardinality budgets per tenant, namespace, and metric,
    so that one bad label cannot exhaust the whole deployment.
18. As an operator, I want budget violations to be observable, so that rejected
    or quarantined series can be diagnosed.
19. As an operator, I want partial batch acceptance when possible, so that one
    invalid series does not necessarily drop an entire remote-write request.
20. As an operator, I want RedDB to avoid silent label dropping, so that series
    semantics are not changed behind my back.
21. As a platform engineer, I want tenant identity to come from auth/header
    context instead of an ordinary metric label, so that tenant isolation is a
    security boundary rather than a dashboard convention.
22. As a platform engineer, I want namespace to be part of series identity, so
    that teams can isolate metrics within one RedDB deployment.
23. As a security engineer, I want policies and RLS to gate metrics query access,
    so that Grafana users only see metrics they are allowed to inspect.
24. As a security engineer, I want metrics ingestion and query activity audited
    at tenant/namespace granularity, so that operational access is reviewable.
25. As a developer, I want SQL access to the same metrics stored for Grafana, so
    that RedDB can support richer investigations than PromQL alone.
26. As a developer, I want RedDB metrics to be joinable with events later, so
    that deploy events, errors, and latency changes can be correlated.
27. As a product engineer, I want the core metrics model to be RedDB-native, so
    that future RedDB-only analytics are not constrained by Prometheus internals.
28. As a customer migrating from Prometheus, I want a compatibility matrix, so
    that I know which dashboards and PromQL functions are supported in v0.
29. As a customer migrating from Prometheus, I want unsupported PromQL features
    to fail clearly, so that bad panels are easy to fix.
30. As a release maintainer, I want a Grafana smoke dashboard in CI or release
    validation, so that compatibility does not regress silently.
31. As a release maintainer, I want remote-write fixture tests, so that the
    adapter keeps accepting real Prometheus wire payloads.
32. As a release maintainer, I want query-result golden tests against simple
    Prometheus-equivalent fixtures, so that practical PromQL semantics stay
    stable.
33. As an SRE, I want RedDB to support gauges, so that CPU, memory, queue depth,
    and replica lag panels work.
34. As an SRE, I want RedDB to support counters, so that requests, errors, and
    retries are queryable.
35. As an SRE, I want RedDB to support classic histogram bucket, count, and sum
    series, so that latency dashboards work without native histogram support.
36. As an operator, I want native Prometheus histograms documented as out of
    scope for v0 unless a customer fixture requires them, so that expectations
    are explicit.
37. As a product owner, I want alert events via queues tracked as a later phase,
    so that the metrics backend can eventually replace more of Prometheus and
    Alertmanager without bloating the first milestone.
38. As a product owner, I want a native Grafana plugin tracked as a later phase,
    so that RedDB-only analytics can emerge after Prometheus datasource
    compatibility proves useful.

## Implementation Decisions

- **Create a metrics model, not just generic time-series rows.** The user-facing
  model should be something like `CREATE METRICS`, backed by time-series chunks,
  retention, hypertables, and continuous aggregates. It must carry
  metrics-specific contracts: metric kind, tenant, namespace, normalized label
  set, fingerprint/series id, staleness, rollup policy, cardinality policy, and
  histogram handling.
- **Keep Prometheus in the adapter.** Prometheus `remote_write`, Prometheus HTTP
  query API shapes, PromQL parsing, and Prometheus-specific response formats
  live at the compatibility boundary. The engine exposes native metrics
  concepts and native query plans.
- **Grafana compatibility first.** The first connector is the built-in Grafana
  Prometheus datasource pointed at RedDB. A native Grafana datasource plugin is
  explicitly deferred.
- **Remote write first.** RedDB receives Prometheus `remote_write`. A RedDB
  scraper with service discovery, relabeling, target health, TLS, and retry
  policy is out of scope for v0.
- **PromQL subset first.** v0 supports selectors, range selectors, common
  aggregations, `rate`, `irate`, `increase`, simple arithmetic, grouping, and
  `histogram_quantile`. It does not promise full PromQL compatibility.
- **Use a native metrics logical plan.** The Prometheus adapter should compile
  supported PromQL into a `MetricsLogicalPlan` or equivalent native plan, not
  generate SQL strings. SQL metrics queries can later compile into the same
  native plan.
- **Classic histograms are in scope.** `_bucket`, `_sum`, and `_count` series
  plus `histogram_quantile` are required for useful SRE dashboards. Prometheus
  native histograms are deferred unless customer fixtures require them.
- **Layered retention is in scope.** Raw TTL plus rollups by resolution are part
  of the v0 product contract, not a later optimization.
- **Cardinality budgets are first-class.** RedDB rejects or quarantines new
  series that exceed budget. It does not silently drop labels or merge series.
- **Tenant and namespace are part of series identity.** Tenant is resolved from
  auth/header context and is not modeled as an ordinary Prometheus label.
- **Alert events use queues later.** Queues are the right mechanism for alert
  state-change delivery, retries, fanout, and DLQ, but that is a phase 2
  alerting feature. Queues are not the primary metrics ingestion API.
- **Study upstreams under `.study`.** Prometheus and Grafana should be cloned
  into ignored `.study/upstream/` directories for compatibility research, with
  pinned tags and a written matrix. Code should not be copied into the product.

Likely deep modules:

- **Metrics catalog.** Owns metric collections, tenant/namespace mapping,
  metric kind, rollup policy, retention policy, and cardinality budgets.
- **Series registry.** Owns normalized label sets, fingerprinting, series ids,
  creation/admission, and cardinality budget enforcement.
- **Remote-write decoder.** Owns Prometheus wire decoding, validation, partial
  acceptance, and conversion into RedDB metric batches.
- **Metrics ingest pipeline.** Owns durable batching, backpressure, optional
  internal queueing, WAL integration, chunk routing, and ingestion stats.
- **Histogram engine.** Owns classic histogram bucket ingestion and
  `histogram_quantile` query semantics. Future sketches/T-Digest can extend it.
- **Rollup engine.** Owns raw-to-rollup materialization and automatic rollup
  selection for query ranges.
- **PromQL adapter.** Owns parsing the supported subset and compiling it into
  a metrics logical plan.
- **Metrics executor.** Executes metrics logical plans against raw and rollup
  storage while applying tenant/RLS filters, staleness, lookback, and counter
  reset semantics.
- **Prometheus HTTP API adapter.** Owns `/api/v1/query`, `/api/v1/query_range`,
  status/error envelopes, and Grafana-compatible response shapes.

## Testing Decisions

Good tests should verify externally observable behavior: remote-write payloads
ingest, Grafana-shaped Prometheus API calls return the expected matrix/vector
responses, unsupported PromQL fails clearly, TTL removes old raw samples, and
long queries select rollups without changing visible results beyond documented
resolution.

Priority coverage:

- **Remote-write fixtures.** Decode real Prometheus remote-write payloads and
  assert accepted samples, rejected series, partial acceptance, and error
  envelopes.
- **Series registry and cardinality budgets.** Unit/property tests for label
  normalization, fingerprint stability, tenant/namespace separation, and budget
  rejection/quarantine.
- **Metrics ingest pipeline.** Integration tests for single-sample, bulk batch,
  WAL/reopen, duplicate/retry behavior, and backpressure counters.
- **PromQL subset.** Golden tests for selectors, range selectors, `rate`,
  `increase`, grouping aggregations, simple arithmetic, and unsupported feature
  errors.
- **Histogram queries.** Fixtures for classic histogram buckets and expected
  `histogram_quantile` outputs for p50, p95, and p99.
- **Retention and rollups.** Tests that raw TTL expires while rollups remain,
  and that long query ranges use rollup data.
- **Tenant/RLS.** Tests proving one tenant cannot query another tenant's series
  even if it guesses labels or metric names.
- **Grafana smoke.** A release smoke test that points Grafana's Prometheus
  datasource at RedDB and renders a small dashboard using common panels.
- **Compatibility matrix.** Documented fixtures for supported and unsupported
  PromQL/dashboard features, updated alongside adapter changes.

Prior art in this repository includes time-series integration tests, continuous
aggregate tests, queue/event backpressure and DLQ tests, red-schema
compatibility tests, and existing `/metrics` parser/doctor tests.

## Out of Scope

- Full Prometheus server clone.
- RedDB scraper, service discovery, target relabeling, and target health.
- Prometheus Alertmanager replacement.
- Recording rules and alerting rules engine.
- Native Grafana datasource plugin.
- Full PromQL compatibility.
- Prometheus native histograms, unless a customer fixture makes them mandatory
  for v0.
- Bit-for-bit equivalence with every Prometheus staleness edge case.
- Silent label dropping as a cardinality-control mechanism.
- Queue-based metrics ingestion as the public compatibility API.

## Further Notes

This PRD intentionally follows the existing RedDB adapter discipline: external
compatibility translates at the boundary; the engine remains RedDB-native. The
same principle is already documented for wire adapters and should apply here.

The first success criterion is not "RedDB is better than Prometheus at
everything." It is narrower: a customer can remove Prometheus as the metrics
backend for Grafana dashboards by pointing collectors and Grafana at RedDB, with
documented limitations and reliable RedDB-native storage behavior.

The second success criterion is product differentiation: after compatibility is
working, RedDB can expose SQL, events, cohorts, document joins, tenant-aware
policies, and richer statistics on top of the same metrics data.
