# Migrating Prometheus/Grafana Metrics To RedDB

RedDB Metrics v0 is a Prometheus-compatible backend for remote-write ingest and
Grafana panel reads. The migration path keeps collectors and Grafana's built-in
Prometheus datasource, then points their write/read URLs at RedDB.

This guide covers the Metrics collection compatibility surface. It is not the
Analytics v0 descriptor catalog; product metrics and KPI/SLI definitions should
continue to name ordinary RedDB collections or source profiles.

## RedDB Setup

Start RedDB and create a metrics collection:

```sql
CREATE METRICS sre RETENTION 30 d DOWNSAMPLE 60s:raw:avg
```

Use `X-RedDB-Tenant` and `X-RedDB-Namespace` headers when a deployment serves
multiple tenants or environments. These become internal series identity fields;
they are not ordinary Prometheus labels and are not returned to Grafana.

## Prometheus Remote Write

Add RedDB as a remote-write target:

```yaml
remote_write:
  - url: http://reddb.example.com:8080/api/v1/write?collection=sre
    headers:
      X-RedDB-Tenant: acme
      X-RedDB-Namespace: prod
```

Keep scrape jobs unchanged during the first migration. Once RedDB dashboards are
validated, reduce Prometheus retention or move Prometheus to edge scraping only.

## Grafana Alloy

Grafana Alloy can scrape and forward Prometheus samples:

```hcl
prometheus.remote_write "reddb" {
  endpoint {
    url = "http://reddb.example.com:8080/api/v1/write?collection=sre"
    headers = {
      "X-RedDB-Tenant" = "acme"
      "X-RedDB-Namespace" = "prod"
    }
  }
}

prometheus.scrape "app" {
  targets    = [{"__address__" = "app:9100"}]
  forward_to = [prometheus.remote_write.reddb.receiver]
}
```

## OpenTelemetry Collector

Use the Prometheus receiver plus Prometheus remote-write exporter:

```yaml
receivers:
  prometheus:
    config:
      scrape_configs:
        - job_name: app
          static_configs:
            - targets: ["app:9100"]

exporters:
  prometheusremotewrite:
    endpoint: http://reddb.example.com:8080/api/v1/write?collection=sre
    headers:
      X-RedDB-Tenant: acme
      X-RedDB-Namespace: prod

service:
  pipelines:
    metrics:
      receivers: [prometheus]
      exporters: [prometheusremotewrite]
```

## Grafana Datasource

Configure Grafana's Prometheus datasource URL as the RedDB HTTP base URL:

```yaml
url: http://reddb.example.com:8080
type: prometheus
access: proxy
```

For tenant-scoped deployments, configure datasource HTTP headers:

- `X-RedDB-Tenant: acme`
- `X-RedDB-Namespace: prod`

## v0 Supported Queries

- Selectors: `metric`, `metric{label="value"}`, `metric{label!="value"}`.
- Range queries via `/api/v1/query_range`.
- Counter functions: `rate`, `irate`, `increase`.
- Aggregations: `sum`, `avg`, `min`, `max`, `count` with `by`/`without`.
- Scalar and vector/scalar arithmetic.
- Classic histogram buckets with `histogram_quantile`.
- Raw retention plus rollup-backed long ranges.
- Cardinality rejection counters in `/metrics`.
- Tenant/namespace isolation through request/auth context.

## v0 Limitations

- No scraping, service discovery, or target health management inside RedDB.
- No Alertmanager replacement or recording rules engine.
- No Prometheus native histogram support.
- No regex label matchers, vector/vector matching, `on`, `ignoring`,
  `group_left`, or `group_right`.
- No `/api/v1/labels`, `/api/v1/label/<name>/values`, `/api/v1/series`, or
  `/api/v1/metadata` compatibility yet.
- No Prometheus stale marker/lookback parity beyond strict function windows.

Run the smoke in [Metrics Grafana smoke](metrics-grafana-smoke.md) before
declaring a dashboard migrated.
