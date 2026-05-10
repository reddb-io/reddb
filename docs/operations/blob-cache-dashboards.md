# Blob Cache observability: dashboards, queries, alerts

Status: 2026-05-06 — first cut. Companion to
[ADR 0006 — Tiered Blob Cache](../adr/0006-tiered-blob-cache.md),
[ADR 0010 — Serialization boundary discipline](../adr/0010-serialization-boundary-discipline.md),
and [Cache vs KV vs Redis](../guides/cache-comparison.md).

This page is for operators who already have RedDB shipping Prometheus
metrics and want a starting point for Blob Cache dashboards, alerts, and
on-call response. It is intentionally partial: panel snippets, not a
full Grafana export. Readers are expected to paste the queries into
their own dashboards and tune thresholds against their own baselines.

The metric source of truth is
[`crates/reddb-server/src/storage/cache/blob/cache.rs`](../../crates/reddb-server/src/storage/cache/blob/cache.rs)
(`BlobCacheStats` and related metric constants re-exported from `blob/mod.rs`). When this doc and
the source disagree, the source wins — file an issue and update this
page.

Backup and restore concerns for the L2 spill directory are tracked
separately in [`blob-cache-backup-restore.md`](./blob-cache-backup-restore.md)
(in flight via Lane #187).

---

## 1. Metric inventory

All metrics emit through structured fields per ADR 0010 — no string
concatenation, no hand-formatted labels. The `namespace` label is the
only high-cardinality dimension and is bounded (see §5).

| Metric | Type | Labels | Unit | Source |
|---|---|---|---|---|
| `cache_blob_l1_bytes_in_use` | gauge | `namespace` | bytes | `METRIC_CACHE_BLOB_L1_BYTES_IN_USE` |
| `cache_blob_l1_bytes_max` | gauge | — | bytes | `BlobCacheStats.l1_bytes_max` |
| `cache_blob_l1_hits_total` | counter | `namespace` | events | `BlobCacheStats.hits` |
| `cache_blob_l1_misses_total` | counter | `namespace` | events | `BlobCacheStats.misses` |
| `cache_blob_l1_evictions_total` | counter | `namespace` | events | `BlobCacheStats.evictions` |
| `cache_blob_l1_entries` | gauge | `namespace` | entries | `BlobCacheStats.entries` |
| `cache_blob_namespaces` | gauge | — | namespaces | `BlobCacheStats.namespaces` |
| `cache_blob_namespaces_max` | gauge | — | namespaces | `BlobCacheStats.max_namespaces` |
| `reddb_cache_blob_l2_bytes_in_use` | gauge | `namespace` | bytes | `METRIC_CACHE_BLOB_L2_BYTES_IN_USE` |
| `reddb_cache_blob_l2_bytes_max` | gauge | — | bytes | `BlobCacheStats.l2_bytes_max` |
| `reddb_cache_blob_l2_full_rejections_total` | counter | `namespace` | events | `METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL` |
| `cache_version_mismatch_total` | counter | `namespace` | events | `METRIC_CACHE_VERSION_MISMATCH_TOTAL` |

The `cache_blob_l1_*` family carries the legacy (un-prefixed) names from
the L1-only tracer; the `reddb_cache_blob_l2_*` family adopts the project
prefix introduced when L2 landed. This split is intentional and stable
— do not rename in place.

---

## 2. PromQL recipes

All queries below are valid PromQL and have been linted against
Prometheus 2.48. Substitute `$namespace` with a Grafana template
variable (or drop the label matcher for the global view).

### 2.1 L1 hit rate (5m window)

```promql
sum(rate(cache_blob_l1_hits_total[5m]))
  /
(sum(rate(cache_blob_l1_hits_total[5m])) + sum(rate(cache_blob_l1_misses_total[5m])))
```

A healthy hit rate depends on the workload. Result-cache traffic
typically sits above 80%; cold-start or scan-heavy traffic dips lower.
Trend matters more than the absolute number.

### 2.2 L1 saturation

```promql
sum(cache_blob_l1_bytes_in_use) / scalar(max(cache_blob_l1_bytes_max))
```

`cache_blob_l1_bytes_max` is reported as a gauge so the panel adapts
when operators bump `RED_BLOB_L1_BYTES_MAX` without redeploying the
dashboard.

### 2.3 L2 saturation

```promql
sum(reddb_cache_blob_l2_bytes_in_use) / scalar(max(reddb_cache_blob_l2_bytes_max))
```

### 2.4 Eviction pressure

```promql
sum(rate(cache_blob_l1_evictions_total[5m]))
```

A non-zero rate is normal under SIEVE; a rate that tracks 1:1 with
`misses_total` means the working set exceeds L1 capacity.

### 2.5 L2 full rejections

```promql
sum(rate(reddb_cache_blob_l2_full_rejections_total[5m]))
```

Any sustained non-zero value is a capacity signal. Either the L2
budget is too small or sweep is falling behind.

### 2.6 Version mismatch (CAS contention)

```promql
sum(rate(cache_version_mismatch_total[5m]))
```

Spikes here indicate writers racing on the same key. Investigate
producer fan-out before raising L2.

### 2.7 Per-namespace breakdown

Any of the above with `by (namespace)`. Example — top 10 namespaces
by L1 bytes:

```promql
topk(10, sum by (namespace) (cache_blob_l1_bytes_in_use))
```

Per-namespace hit rate:

```promql
sum by (namespace) (rate(cache_blob_l1_hits_total[5m]))
  /
(sum by (namespace) (rate(cache_blob_l1_hits_total[5m])) + sum by (namespace) (rate(cache_blob_l1_misses_total[5m])))
```

---

## 3. Alert rules

Drop the YAML below into your Prometheus rules file. Severities follow
the `info` / `warning` / `critical` convention; only `warning` paged
alerts page the operator on-call. Tune `for:` durations against your
own change windows.

```yaml
groups:
  - name: blob-cache
    interval: 30s
    rules:
      - alert: BlobCacheL2FullRejectionsAlert
        expr: sum(rate(reddb_cache_blob_l2_full_rejections_total[10m])) > 0.1
        for: 5m
        labels:
          severity: warning
          component: blob-cache
        annotations:
          summary: "Blob Cache L2 rejecting writes"
          description: |
            L2 spill is rejecting >0.1 writes/sec for 5m. Capacity, sweep,
            or both are unhealthy. Page on-call.
          runbook: docs/operations/blob-cache-dashboards.md#3-alert-rules

      - alert: BlobCacheL1Saturated
        expr: sum(cache_blob_l1_bytes_in_use) / scalar(max(cache_blob_l1_bytes_max)) > 0.95
        for: 15m
        labels:
          severity: warning
          component: blob-cache
        annotations:
          summary: "Blob Cache L1 above 95%"
          description: |
            L1 sustained >95% for 15m. Eviction pressure will dominate
            and tail latency will rise. Consider raising RED_BLOB_L1_BYTES_MAX.

      - alert: BlobCacheHitRateLow
        expr: |
          (sum(rate(cache_blob_l1_hits_total[30m]))
            /
           (sum(rate(cache_blob_l1_hits_total[30m])) + sum(rate(cache_blob_l1_misses_total[30m]))))
          < 0.5
        for: 30m
        labels:
          severity: info
          component: blob-cache
        annotations:
          summary: "Blob Cache hit rate below 50%"
          description: |
            L1 hit rate <50% for 30m. Workload may be scan-heavy or the
            cache may be undersized. No paging — informational only.

      - alert: BlobCacheVersionMismatchSpike
        expr: sum(rate(cache_version_mismatch_total[10m])) > 1
        for: 10m
        labels:
          severity: info
          component: blob-cache
        annotations:
          summary: "CAS contention on Blob Cache"
          description: |
            Version mismatch >1/sec for 10m. Likely concurrent writers
            on the same keys. Investigate producer fan-out.

      - alert: BlobCacheNamespaceCardinality
        expr: max(cache_blob_namespaces) > 200
        for: 5m
        labels:
          severity: warning
          component: blob-cache
        annotations:
          summary: "Namespace count approaching MVP cap"
          description: |
            Blob Cache holds >200 namespaces. MVP cap is 256 (#144).
            Plan capacity or accept rejected admissions.
```

Severity totals: 2 × `warning` (paging), 1 × `warning` (capacity), 2 × `info`.

---

## 4. Grafana dashboard panels

The snippet below is a partial dashboard fragment — seven panels, one
per scenario, with `${DS_PROMETHEUS}` as the datasource placeholder.
Paste into a new dashboard JSON model under `panels: [...]` and adjust
`gridPos` to your layout.

```json
{
  "panels": [
    {
      "title": "L1 hit rate (5m)",
      "type": "stat",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "percentunit", "min": 0, "max": 1 } },
      "targets": [
        {
          "expr": "sum(rate(cache_blob_l1_hits_total[5m])) / (sum(rate(cache_blob_l1_hits_total[5m])) + sum(rate(cache_blob_l1_misses_total[5m])))",
          "legendFormat": "hit rate",
          "refId": "A"
        }
      ]
    },
    {
      "title": "L1 saturation",
      "type": "gauge",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "percentunit", "min": 0, "max": 1 } },
      "targets": [
        {
          "expr": "sum(cache_blob_l1_bytes_in_use) / scalar(max(cache_blob_l1_bytes_max))",
          "legendFormat": "L1 used",
          "refId": "A"
        }
      ]
    },
    {
      "title": "L2 saturation",
      "type": "gauge",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "percentunit", "min": 0, "max": 1 } },
      "targets": [
        {
          "expr": "sum(reddb_cache_blob_l2_bytes_in_use) / scalar(max(reddb_cache_blob_l2_bytes_max))",
          "legendFormat": "L2 used",
          "refId": "A"
        }
      ]
    },
    {
      "title": "L1 eviction pressure",
      "type": "timeseries",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "ops" } },
      "targets": [
        {
          "expr": "sum(rate(cache_blob_l1_evictions_total[5m]))",
          "legendFormat": "evictions/s",
          "refId": "A"
        }
      ]
    },
    {
      "title": "L2 full rejections",
      "type": "timeseries",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "ops" } },
      "targets": [
        {
          "expr": "sum(rate(reddb_cache_blob_l2_full_rejections_total[5m]))",
          "legendFormat": "rejections/s",
          "refId": "A"
        }
      ]
    },
    {
      "title": "Version mismatch (CAS contention)",
      "type": "timeseries",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "ops" } },
      "targets": [
        {
          "expr": "sum(rate(cache_version_mismatch_total[5m]))",
          "legendFormat": "mismatches/s",
          "refId": "A"
        }
      ]
    },
    {
      "title": "Top namespaces by L1 bytes",
      "type": "timeseries",
      "datasource": { "type": "prometheus", "uid": "${DS_PROMETHEUS}" },
      "fieldConfig": { "defaults": { "unit": "bytes" } },
      "targets": [
        {
          "expr": "topk(10, sum by (namespace) (cache_blob_l1_bytes_in_use))",
          "legendFormat": "{{namespace}}",
          "refId": "A"
        }
      ]
    }
  ]
}
```

The JSON is parseable by `grafana-cli` and `jq` as-is. Panel IDs,
`gridPos`, and dashboard-level metadata are left to the importer
because they collide with whatever shell dashboard the operator
already maintains.

---

## 5. Cardinality budget

Per the #144 deepening, namespace cardinality is bounded at **256** in
MVP (`DEFAULT_BLOB_MAX_NAMESPACES`). Every per-namespace
metric in this doc multiplies series count by current namespace count,
so the worst-case Blob Cache series budget today is roughly:

```
12 metrics × 256 namespaces ≈ 3,000 active series per RedDB node
```

That fits comfortably in any modern Prometheus deployment. The
`BlobCacheNamespaceCardinality` alert in §3 fires before the cap
becomes a hard rejection on admission.

**If the cap is ever raised** toward per-tenant cardinality (10k+
namespaces), this observability layout needs a redesign:

1. The default panels should switch to **rolled-up** metrics — sum,
   topk, quantile — without the `namespace` label.
2. Per-namespace breakdowns move to **on-demand admin queries**
   (`/admin/cache/blob/stats?namespace=…`), not Prometheus series.
3. Alert thresholds re-baseline against the new fleet shape.

The current numbers assume the MVP regime. Treat this section as the
trigger to reopen the design — do not silently accept the series
explosion.

---

## See also

- ADR 0006 — Tiered Blob Cache: [`docs/adr/0006-tiered-blob-cache.md`](../adr/0006-tiered-blob-cache.md)
- ADR 0010 — Serialization boundary discipline: [`docs/adr/0010-serialization-boundary-discipline.md`](../adr/0010-serialization-boundary-discipline.md)
- Cache comparison guide: [`docs/guides/cache-comparison.md`](../guides/cache-comparison.md)
- Blob Cache backup/restore (Lane #187): [`docs/operations/blob-cache-backup-restore.md`](./blob-cache-backup-restore.md)
- Metric source: [`crates/reddb-server/src/storage/cache/blob/cache.rs`](../../crates/reddb-server/src/storage/cache/blob/cache.rs)
