# Analytics v0 Ontology

Analytics v0 is a metric-centric catalog. It does not introduce a generic
`ANALYTICS` object, an analytics collection type, or a second write path for
raw application data.

The central object is a **metric**: a named measure with a stable definition.
A metric records what is being measured, where the source data comes from, what
unit and kind it has, which dimensions are allowed, and which role it plays in
review and operations.

## Core Contract

Analytics v0 has these boundaries:

| Concept | v0 meaning | Not this |
|:--------|:-----------|:---------|
| Analytics | Product area for metrics, roles, objectives, and derived analytical reads | A generic `CREATE ANALYTICS` object |
| Metric | Named measure over ordinary RedDB data or a materialized sample stream | A raw-data collection |
| KPI | Metric role for product or business outcome review | Separate storage primitive |
| SLI | Metric role for service-quality review | Separate storage primitive |
| SLO | Objective over an SLI metric, with target and window | Metric role or raw metric sample |
| TimeSeries | Storage/layout for timestamped samples and materializations | The analytics catalog itself |
| Probabilistic structures | Approximate execution sidecars for specific questions | Source-of-truth metric storage |

## Raw Data Writes

Raw source data remains in ordinary RedDB collections:

- product events can be table or document rows
- operational telemetry can arrive through the Prometheus remote-write adapter
  for a metrics collection
- application state remains in tables, documents, KV, graph, vectors, queues,
  or time-series according to the source model

Analytics v0 metrics refer to those sources. They do not replace them.

`INSERT INTO METRIC` is explicitly out of scope for Analytics v0. If a future
release adds direct metric ingest sugar, it must be specified as sugar over a
normal sample collection or adapter path, not as a new raw-data storage model.

## Metric Descriptors

A metric descriptor is catalog state keyed by a stable metric path such as
`infra.database.cpu.usage` or `product.checkout.conversion_rate`.

The descriptor should carry:

- metric kind, such as counter, gauge, histogram, ratio, or derived value
- unit
- allowed dimensions and cardinality policy
- source collection, query, adapter, or materialization plan
- retention or rollup policy when samples are materialized
- role, such as `kpi`, `sli`, or ordinary metric

Descriptor state must be WAL-backed, selectable/updatable through normal RedDB
policy rules, and reviewable as catalog data. SQL sugar such as
`CREATE METRIC <path> ...` can exist later, but the contract is the catalog
record, not a hidden analytics object.

## KPI, SLI, And SLO

KPI and SLI are roles on metrics:

- A KPI is a metric used for product, business, or operational outcome review.
- An SLI is a metric used to judge service quality.

An SLO is different. It is an objective over an SLI metric, with a target and
window. For example:

```text
metric: infra.api.request_success_ratio
role: sli
slo: 99.9% over 30d
```

That keeps the measured value and the commitment separate. Many SLOs may refer
to the same SLI metric with different tenants, windows, or policy metadata.

## Time-Series Boundary

Time-Series is the storage and query layout for timestamped samples. Analytics
v0 may use time-series chunks, hypertables, retention, rollups, and continuous
aggregates to store or materialize metric values.

Time-Series does not define whether a value is a KPI, SLI, or ordinary metric.
That meaning belongs to the metric descriptor and any SLO catalog records above
it.

## Probabilistic Boundary

Probabilistic structures answer approximate questions cheaply:

- HyperLogLog estimates distinct counts.
- Count-Min Sketch estimates frequencies.
- Cuckoo Filter answers approximate membership.

Analytics v0 can use these structures as execution sidecars for metrics such
as unique users, hot keys, or approximate membership. They are not authoritative
source collections for metrics, and reviewers should reject implementations
that hide metric truth only inside an approximate structure.

## Implementation Review Checklist

Use this checklist when reviewing Analytics v0 runtime work:

- Raw writes still enter ordinary RedDB collection, adapter, or time-series
  paths; no `INSERT INTO METRIC`.
- Metric descriptors are catalog records with stable names, sources, roles,
  dimensions, and policy metadata.
- KPI and SLI are roles on metrics.
- SLO is a separate objective over an SLI metric.
- Time-Series is used for sample storage/layout, not as the ontology boundary.
- Probabilistic structures are sidecars or summaries, not source-of-truth
  metric storage.

## See Also

- [Metrics](./metrics.md)
- [Time-Series](./timeseries.md)
- [Probabilistic Structures](./probabilistic.md)
- [Data Model Overview](./overview.md)
