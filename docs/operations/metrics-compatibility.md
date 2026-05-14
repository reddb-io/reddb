# RedDB Metrics Prometheus/Grafana Compatibility Matrix

**Status:** v0 implementation contract
**Date:** 2026-05-14
**Related:** [Metrics data model](../data-models/metrics.md), [ADR 0017](../adr/0017-prometheus-grafana-adapters-for-metrics.md)

This matrix defines the first compatibility target for using RedDB as a
Prometheus-compatible metrics backend and as a Grafana Prometheus datasource
target. It is intentionally narrower than full Prometheus. RedDB owns the
native metrics engine; Prometheus and Grafana shapes are adapter contracts at
the boundary.

## Upstream References

The local study pointers live under ignored `.study/upstream/` paths:

| Upstream | Reference | Pinned point | Use in v0 |
|---|---|---|---|
| Prometheus | `https://github.com/prometheus/prometheus` | tag `v3.11.3`, commit `eb173f5256d4022afba1e9bc3d19740a76859fae` | Remote-write sender behavior, HTTP API shape, PromQL semantics |
| Grafana | `https://github.com/grafana/grafana` | tag `v13.0.1+security-01`, commit `9bbe672d13753e132db266e1f47dcaf362a76e81` | Built-in Prometheus datasource behavior and dashboard/query-editor expectations |

Normative compatibility references:

- Prometheus remote-write 1.0 specification:
  `https://prometheus.io/docs/specs/prw/remote_write_spec/`
- Prometheus HTTP API:
  `https://prometheus.io/docs/prometheus/latest/querying/api/`
- Prometheus querying basics, functions, and operators:
  `https://prometheus.io/docs/prometheus/latest/querying/basics/`
  `https://prometheus.io/docs/prometheus/latest/querying/functions/`
  `https://prometheus.io/docs/prometheus/latest/querying/operators/`
- Grafana Prometheus datasource and query editor:
  `https://grafana.com/docs/grafana/latest/datasources/prometheus/`
  `https://grafana.com/docs/grafana/latest/datasources/prometheus/query-editor/`

## Compatibility Goal

v0 must support this migration path:

1. A customer keeps an existing Prometheus-compatible collector or Prometheus
   instance.
2. The collector sends samples to RedDB by `remote_write`.
3. Grafana's built-in Prometheus datasource points at RedDB.
4. Common SRE dashboards for rates, gauges, grouped aggregations, and classic
   histogram percentiles render without query rewrites.

v0 is not a full Prometheus server replacement. It is a RedDB metrics backend
with Prometheus-compatible ingest and query adapters.

## Endpoint Matrix

| Endpoint | Method | v0 status | Contract |
|---|---:|---|---|
| `/api/v1/write` | `POST` | Required | Prometheus remote-write 1.0 receiver for counters, gauges, and classic histogram series |
| `/api/v1/query` | `GET`, `POST` | Required | Instant Prometheus query API for supported PromQL subset |
| `/api/v1/query_range` | `GET`, `POST` | Required | Range Prometheus query API for Grafana time-series panels |
| `/api/v1/labels` | `GET`, `POST` | Required | Label-name discovery for Grafana variables and autocomplete |
| `/api/v1/label/{name}/values` | `GET`, `POST` | Required | Label-value discovery for Grafana variables and query builder |
| `/api/v1/series` | `GET`, `POST` | Required | Series discovery for Grafana metrics browser and smoke tests |
| `/api/v1/metadata` | `GET` | Best effort | Metadata autocomplete when metric type/help is available; empty success is allowed |
| `/api/v1/status/buildinfo` | `GET` | Best effort | Health/probe compatibility; may return RedDB build metadata in Prometheus envelope |
| `/metrics` | `GET` | Existing | RedDB operator self-metrics; not customer metrics query API |
| Prometheus scraping APIs | mixed | Out of scope | RedDB v0 does not scrape targets or manage service discovery |
| Remote read | `POST` | Out of scope | Grafana path uses HTTP query API, not remote read |
| Alertmanager/rule APIs | mixed | Out of scope | Grafana alerting may query RedDB; RedDB-native rule evaluation is later |
| Admin/TSDB/federation APIs | mixed | Out of scope | No compatibility promise in v0 |

## Remote-Write Ingest Contract

`POST /api/v1/write` accepts Prometheus remote-write 1.0 requests:

- Body: Snappy block-compressed protobuf `WriteRequest`.
- Required headers:
  - `Content-Encoding: snappy`
  - `Content-Type: application/x-protobuf`
  - `X-Prometheus-Remote-Write-Version: 0.1.0`
- Sample timestamps are milliseconds since Unix epoch.
- Sample values are `float64`.
- Each series must include a valid `__name__` label.
- Repeated label names, empty label names, invalid metric names, and invalid
  label names are rejected.
- Labels are normalized into RedDB series identity:
  `tenant_id + namespace + metric_name + normalized_label_set`.
- `tenant_id` comes from RedDB auth/request context, not from a Prometheus
  label.
- `namespace` comes from the metrics collection or adapter configuration.

Successful writes return `2xx` with an empty body. `204 No Content` is the
preferred success code.

Failure behavior:

| Case | HTTP status | Retry expectation | Notes |
|---|---:|---|---|
| Malformed Snappy/protobuf or missing required headers | `400` | Do not retry | Request cannot become valid unchanged |
| Invalid series/sample labels | `400` | Do not retry | RedDB may ingest valid samples from the request only if the rejection is recorded; sender must not rely on response-body details |
| Cardinality policy rejects new series permanently | `400` | Do not retry | Rejection/quarantine counters must explain the policy hit |
| Temporary cardinality/backpressure throttle | `429` | May retry | Use when retry can succeed after pressure drops |
| Storage unavailable, WAL failure, or transient internal error | `5xx` | Retry | Must only be returned before RedDB has durably accepted the batch |

For v0, a successful response means every accepted sample has crossed RedDB's
durability boundary. WAL ordering and crash recovery semantics are part of the
storage implementation contract, not the HTTP adapter.

## Query API Contract

Supported query endpoints return Prometheus-shaped JSON envelopes:

```json
{
  "status": "success",
  "data": {
    "resultType": "vector",
    "result": []
  }
}
```

Errors return Prometheus-shaped JSON:

```json
{
  "status": "error",
  "errorType": "bad_data",
  "error": "unsupported PromQL feature: vector matching"
}
```

Error status rules:

| Case | HTTP status | `errorType` |
|---|---:|---|
| Missing or invalid request parameters | `400` | `bad_data` |
| PromQL parses but uses unsupported v0 semantics | `422` | `execution` |
| Query timeout, cancellation, or resource limit | `503` | `timeout` or `canceled` |
| Internal adapter/engine failure | `500` | `internal` |

Unsupported PromQL must fail explicitly. It must not return partial or
silently rewritten results that look correct.

## PromQL v0 Subset

| Area | Supported in v0 | Explicitly out of scope in v0 |
|---|---|---|
| Selectors | Instant-vector selectors, range-vector selectors, metric names, `{label="value"}`, `{label!="value"}`, regex matchers `{label=~"re"}` and `{label!~"re"}` | Subqueries, `@` modifiers, broad compatibility with every Prometheus grammar edge |
| Time windows | Range selectors like `[1m]`, `[5m]`, `[1h]` | Prometheus engine lookback tuning as a public knob |
| Counter functions | `rate`, `irate`, `increase` with reset handling | Full native-histogram function behavior |
| Histogram functions | `histogram_quantile` over classic histogram buckets | Native histogram query support |
| Aggregations | `sum`, `avg`, `min`, `max`, `count` | `topk`, `bottomk`, `quantile`, `stddev`, `stdvar`, experimental aggregators |
| Grouping | `by (...)`, `without (...)` | Grouping behavior outside supported aggregators |
| Arithmetic | Scalar/scalar and vector/scalar `+`, `-`, `*`, `/` | Vector/vector matching, `on`, `ignoring`, `group_left`, `group_right`, `%`, `^` |
| Comparisons/logical ops | Out of scope except where needed internally for future filters | Full comparison, `and`, `or`, `unless` |
| Label functions | Out of scope | `label_replace`, `label_join`, and metadata-heavy label transforms |
| Staleness | Ingest stale markers and stop extending dead series | Full Prometheus staleness/lookback compatibility in every edge case |

## Histogram Contract

v0 supports classic Prometheus histograms represented by:

- `<metric>_bucket{le="..."}`
- `<metric>_sum`
- `<metric>_count`

Requirements:

- Buckets remain cumulative.
- `le` is required for bucket series.
- `+Inf` bucket is required for quantile correctness.
- `histogram_quantile(phi, sum by (..., le) (rate(<metric>_bucket[window])))`
  is the primary latency percentile path for p50, p95, and p99 dashboards.
- Native histograms are out of scope unless a customer fixture makes them a v0
  launch blocker.

## Grafana v0 Expectations

Grafana connects through its built-in Prometheus datasource. No custom Grafana
plugin is required in v0.

Expected to work:

- Time series panels backed by `/api/v1/query_range`.
- Stat/gauge/table panels backed by `/api/v1/query`.
- Template variables using label names and label values.
- Query editor code mode for the supported PromQL subset.
- Query builder and metrics browser for metric/label discovery when backed by
  `/labels`, `/label/{name}/values`, `/series`, and optional `/metadata`.
- Grafana alert rules that query supported PromQL through the datasource.

Not promised in v0:

- Exemplars.
- Annotations sourced from Prometheus APIs.
- Native histograms.
- Full autocomplete fidelity for every Prometheus metadata endpoint.
- Grafana datasource settings that assume Prometheus-only admin APIs.
- A custom RedDB datasource plugin.

## Required Fixtures

Downstream implementation issues should create fixtures from these cases.

Remote-write fixtures:

| Fixture | Contents | Purpose |
|---|---|---|
| `remote_write_counter_gauge` | `http_requests_total` counter and `process_resident_memory_bytes` gauge with `job`, `instance`, `tenant`, `service`, `method`, `status` labels | Basic ingest, label normalization, instant/range queries |
| `remote_write_classic_histogram` | `http_request_duration_seconds_bucket`, `_sum`, `_count` with `le` buckets `0.1`, `0.3`, `1`, `+Inf` | p50/p95/p99 dashboard compatibility |
| `remote_write_counter_reset` | Counter series with reset and later samples | `rate`, `irate`, `increase` behavior |
| `remote_write_stale_marker` | Series ending with Prometheus stale marker | Dead-series handling |
| `remote_write_invalid_labels` | Duplicate label, empty label, invalid metric name | `400` rejection behavior |
| `remote_write_cardinality_budget` | Batch introducing more new series than policy allows | permanent reject, throttle, and quarantine semantics |

Grafana query fixtures:

| Fixture | Query | Expected use |
|---|---|---|
| `grafana_selector_panel` | `process_resident_memory_bytes{service="$service"}` | Gauge/stat panel |
| `grafana_rate_panel` | `sum by (service) (rate(http_requests_total{service=~"$service"}[5m]))` | Time-series request-rate panel |
| `grafana_error_rate_panel` | `sum by (service) (rate(http_requests_total{status=~"5.."}[5m]))` | Filtered grouped counter panel |
| `grafana_p95_panel` | `histogram_quantile(0.95, sum by (service, le) (rate(http_request_duration_seconds_bucket[5m])))` | Latency percentile panel |
| `grafana_label_variable` | `/api/v1/label/service/values` | Dashboard variable |
| `grafana_metric_browser` | `/api/v1/series?match[]=http_requests_total` | Query editor/metrics browser |

## Smoke-Dashboard Acceptance

The v0 Grafana smoke test should provision Grafana with a Prometheus datasource
whose URL points at RedDB, load a dashboard, and verify that these panels render
non-empty data:

1. Request rate by service.
2. Error rate by service.
3. Resident memory gauge.
4. p95 request latency from classic histogram buckets.
5. A `$service` template variable populated from RedDB label values.

The smoke test should also verify one unsupported PromQL query returns a
Prometheus-shaped `422` error instead of a silent empty graph.

## Implementation Guardrails

- Prometheus compatibility is an adapter. Do not make PromQL the native RedDB
  query language.
- Query planning should compile supported PromQL into a RedDB metrics logical
  plan.
- Tenant isolation and RLS checks happen before query execution and before
  label discovery responses are rendered.
- Retention, TTL, rollups, and cardinality budgets are RedDB-native policies.
- Bulk ingest and single-sample test helpers must converge on the same internal
  batch path so WAL, ordering, and policy behavior stay identical.
