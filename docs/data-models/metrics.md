# Metrics

Metrics are named measures in RedDB's Analytics v0 ontology. The metric catalog
defines what is being measured, where the source data comes from, what unit and
kind it has, which dimensions are valid, and which role it plays.

RedDB also has a planned Prometheus-compatible observability backend. That
backend stores and queries operational telemetry while keeping the engine model
RedDB-native and aligned with the Analytics v0 metric catalog.

Metrics are related to [Time-Series](./timeseries.md), but they are not just a
generic `metric, value, tags, timestamp` collection. A metrics collection adds
contracts that operational telemetry needs: tenant isolation, label
normalization, series identity, metric kinds, histograms, cardinality budgets,
staleness, TTL, and rollups.

Prometheus compatibility is a Metrics collection and wire surface, not the
Analytics v0 catalog. Analytics v0 can define metric descriptors over Metrics
collections, but descriptor catalog records remain separate from write
protocols and dashboard compatibility.

For the full ontology, including KPI, SLI, SLO, Time-Series, and probabilistic
boundaries, see [Analytics v0 Ontology](./analytics.md).

## Status

The Analytics v0 metric descriptor catalog is distinct from the operational
Metrics collection surface. The existing Metrics compatibility target for
operational telemetry is:

- Prometheus `remote_write` ingestion.
- Prometheus HTTP query API subset for Grafana:
  `/api/v1/query` and `/api/v1/query_range`.
- Grafana using its built-in Prometheus datasource pointed at RedDB.

Those compatibility endpoints feed and query Metrics collections. They are not
the Analytics v0 product API and they are not how arbitrary product metrics,
KPIs, or SLIs are declared.

## When to Use

Use Metrics when you want RedDB to store and query operational telemetry:

- service request rates
- error rates
- CPU and memory gauges
- queue depth
- database health
- p50, p95, and p99 latency from classic histograms
- tenant- or namespace-scoped SRE dashboards

Use [Time-Series](./timeseries.md) directly when you have generic timestamped
measurements and do not need Prometheus/Grafana compatibility semantics.

Use the Analytics v0 metric descriptor catalog when you need named measures over
existing RedDB data, including product KPIs, service SLIs, or derived metrics
sourced from tables, documents, event-shaped collections, Metrics collections,
or materialized queries.

## Raw Data And Source Profiles

Raw data for Analytics v0 stays in ordinary RedDB collections. For example,
product events can be inserted into a table and then described as an analytics
source profile:

```sql
CREATE TABLE product_events (
  ts INTEGER,
  event_name TEXT,
  actor_id TEXT,
  session_id TEXT,
  props TEXT
);

INSERT INTO product_events (ts, event_name, actor_id, session_id, props)
  VALUES (1704067200000000000, 'signup', 'user-1', 'sess-1', '{}');

CREATE ANALYTICS SOURCE product_events ON product_events
  TIME FIELD ts
  EVENT FIELD event_name
  ACTOR FIELD actor_id
  SESSION FIELD session_id
  PROPERTIES FIELD props;
```

The source profile is catalog metadata in `red.analytics.sources`. Metric
descriptors can refer to that source, but the raw write target remains the
backing collection.

## Proposed DDL

The exact syntax is still open, but the product shape is:

```sql
CREATE METRICS sre
  RETENTION RAW 15d
  ROLLUP 1m 30d, 5m 180d, 1h 365d
  CARDINALITY LIMIT 1000000 SERIES;
```

This declares a metrics collection with:

- raw sample retention
- rollup tiers by resolution
- a series-cardinality budget
- metrics-specific ingestion and query behavior

Internally, the implementation should reuse RedDB's time-series chunks,
hypertable routing, retention daemon, batch-write paths, and continuous
aggregate machinery where those primitives fit.

`CREATE METRICS` declares an operational telemetry collection for the
Prometheus-compatible backend. It does not create an Analytics v0 metric
descriptor and it does not replace ordinary collection writes for product or
event data.

## Metric Catalog Roles

Analytics v0 treats KPI and SLI as metric roles:

| Role | Meaning |
|:-----|:--------|
| Ordinary metric | Named measure without a special review role |
| KPI | Metric used for product, business, or operational outcome review |
| SLI | Metric used to judge service quality |

SLO is not a metric role. It is a separate objective over an SLI metric, with a
target and window. Keeping SLOs separate lets multiple objectives refer to the
same measured SLI without changing the metric definition.

## Series Identity

A metric series is identified by:

```text
tenant_id + namespace + metric_name + normalized_label_set
```

`tenant_id` is a security boundary resolved from auth or request context. It is
not an ordinary Prometheus label. `namespace` is an operational grouping inside
a tenant. Labels remain metric dimensions and participate in filtering,
grouping, and cardinality budgeting.

## Metric Kinds

The v0 target supports:

| Kind | Use |
|---|---|
| Counter | Monotonic values such as requests, errors, retries |
| Gauge | Point-in-time values such as memory, CPU, queue depth |
| Classic histogram | Prometheus `_bucket`, `_sum`, and `_count` series |

Classic histograms are required because common Grafana dashboards use
`histogram_quantile()` for latency panels. Prometheus native histograms are a
follow-up unless customer fixtures require them in v0.

## Query Semantics

Prometheus-compatible queries enter through the Prometheus adapter. Supported
PromQL should compile into a native RedDB metrics logical plan rather than into
SQL text.

The v0 PromQL subset should cover:

- metric selectors with label matchers
- range selectors
- `rate`, `irate`, and `increase`
- `sum`, `avg`, `min`, `max`, and `count`
- `by (...)` and `without (...)`
- simple arithmetic
- `histogram_quantile`

Full PromQL compatibility is out of scope for v0. Unsupported features should
fail clearly instead of returning misleading data.

## Retention And Rollups

Metrics should use layered retention:

- raw samples for recent investigation
- 1-minute rollups for medium-range dashboards
- 5-minute or 1-hour rollups for long-range dashboards

Queries should choose the cheapest tier that preserves the requested resolution.
This is where RedDB can be better than a direct Prometheus clone: retention and
rollups are part of the storage contract, not only external recording rules.

## Cardinality Budgets

High-cardinality labels can make metrics systems unstable. RedDB should treat
cardinality budgets as first-class policy:

- per tenant
- per namespace
- per metric
- optionally per label key/value

When a budget is exceeded, RedDB should reject or quarantine new series
explicitly. It should not silently drop labels, because that changes series
semantics and can merge unrelated data.

## Adapters

Prometheus and Grafana compatibility are adapters around the RedDB-native
Metrics collection surface:

- Prometheus `remote_write` becomes RedDB metrics batches.
- Prometheus HTTP API responses are rendered at the adapter boundary.
- Grafana initially connects through its built-in Prometheus datasource.

These adapters are not Analytics v0 behavior. They only translate Prometheus and
Grafana shapes to and from Metrics collections.

## Alerting

Alerting is a later phase. Grafana Alerting can query RedDB through the
Prometheus-compatible datasource in the first milestone.

When RedDB grows native alert rule evaluation, alert state-change events should
be emitted to RedDB queues. Queues are the right tool for notification fanout,
retry, backpressure, and DLQ. They are not the public metrics ingestion API.

## See Also

- [Analytics v0 Ontology](./analytics.md)
- [Time-Series](./timeseries.md)
- [Hypertables](./hypertables.md)
- [Continuous Aggregates](./continuous-aggregates.md)
- [Events](./events.md)
- [Queues](./queues.md)
- [Prometheus and Grafana adapters ADR](../../.red/adr/0017-prometheus-grafana-adapters-for-metrics.md)
- [Prometheus/Grafana compatibility matrix](../operations/metrics-compatibility.md)
