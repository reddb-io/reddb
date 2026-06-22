# RedDB Metrics Grafana Smoke

This smoke validates RedDB as a Grafana Prometheus datasource target for the v0
metrics backend. It complements the automated Rust smoke:

```bash
cargo test --test e2e_metrics_grafana_compat_smoke -- --nocapture
```

## 1. Start RedDB

```bash
make run ARGS='--path ./data/metrics-smoke.db --bind 127.0.0.1:5000'
```

Create the metrics collection:

```sql
CREATE METRICS sre RETENTION 30 d DOWNSAMPLE 60s:raw:avg
```

Ingest fixture data through `POST /api/v1/write?collection=sre` using
Prometheus remote-write snappy protobuf with:

- `Content-Encoding: snappy`
- `Content-Type: application/x-protobuf`
- `X-Prometheus-Remote-Write-Version: 0.1.0`
- optional `X-RedDB-Tenant` and `X-RedDB-Namespace`

The fixture should include:

- `http_requests_total` counters for at least two services and a `5xx` status.
- `process_resident_memory_bytes` gauge.
- `http_request_duration_seconds_bucket`, `_sum`, and `_count` classic
  histogram series.
- One high-cardinality batch that trips the configured cardinality budget.
- One long-range gauge/counter series covered by the declared rollup tier.

## 2. Start Grafana

Use Grafana's built-in Prometheus datasource. The datasource URL is the RedDB
HTTP base URL:

```yaml
apiVersion: 1
datasources:
  - name: RedDB Metrics
    type: prometheus
    access: proxy
    url: http://host.docker.internal:5000
    isDefault: true
    jsonData:
      httpMethod: GET
    secureJsonData: {}
```

If RedDB is tenant-scoped, add datasource HTTP headers:

```yaml
jsonData:
  httpHeaderName1: X-RedDB-Tenant
  httpHeaderName2: X-RedDB-Namespace
secureJsonData:
  httpHeaderValue1: acme
  httpHeaderValue2: prod
```

## 3. Panels

Create panels with these PromQL expressions:

| Panel | Query |
|---|---|
| Request count | `http_requests_total{service="checkout"}` |
| Request rate | `sum by (service) (rate(http_requests_total[20s]))` |
| Error rate | `sum by (service) (rate(http_requests_total{status!="200"}[20s]))` |
| Resident memory | `process_resident_memory_bytes{service="checkout"}` |
| p95 latency | `histogram_quantile(0.95, rate(http_request_duration_seconds_bucket[10s]))` |
| Long range rollup | `long_range_temperature_celsius` with panel step at or above `60s` |

Expected result: each supported panel renders non-empty data. A deliberately
unsupported vector/vector query such as
`rate(http_requests_total[20s]) / rate(http_requests_total[20s])` must return a
Prometheus-shaped error instead of an empty graph.

## 4. Failure Map

| Failure | Likely slice |
|---|---|
| Remote-write rejected before sample validation | #484 |
| Instant selector empty for known data | #485 |
| Range panel shifted or missing steps | #486 |
| `rate`, `irate`, or `increase` wrong after reset | #487 |
| `sum by` / arithmetic / vector matching behavior wrong | #488 |
| `histogram_quantile` wrong or empty for buckets | #489 |
| Cardinality rejection invisible in `/metrics` | #490 |
| Long-range panel scans raw or misses rollup | #491 |
| Cross-tenant data leaks or disappears | #492 |
