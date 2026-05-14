# Metrics Reference

RedDB exposes Prometheus text format at `GET /metrics`.

## Prometheus Metrics Adapter

The metrics adapter exposes the v0 Prometheus HTTP API surface for Grafana:

| Endpoint | Supported v0 shape |
|----------|--------------------|
| `POST /api/v1/write` | Prometheus `remote_write` snappy protobuf samples for counters and gauges. |
| `GET /api/v1/query` | Instant selectors such as `metric_name`, `metric_name{label="value"}`, and counter functions `rate(metric[window])`, `irate(metric[window])`, `increase(metric[window])`. |
| `GET /api/v1/query_range` | Range selectors and the same counter functions with `start`, `end`, and `step`. |

Counter functions use strict v0 window semantics. RedDB evaluates only samples
inside `[evaluation_time - window, evaluation_time]`; if fewer than two samples
exist in that window, the series is absent from the result. Counter resets are
handled by treating a lower value as a reset to zero and adding the post-reset
value to the increase. RedDB does not extrapolate to the full window and does
not emit Prometheus stale markers in v0.

Cardinality admission can be capped with
`REDDB_METRICS_MAX_SERIES_PER_METRIC`. The v0 budget is enforced per
tenant/namespace/metric during `remote_write`. RedDB accepts in-budget series
from a batch and rejects new over-budget series without dropping labels or
merging series. Rejections increment
`reddb_metrics_remote_write_series_rejected_by_reason_total{reason="cardinality_budget"}`.

Metrics collections can declare raw retention and rollup tiers:

```sql
CREATE METRICS sre RETENTION 1 h DOWNSAMPLE 60s:raw:avg
```

The v0 rollup policy syntax is `target:raw:aggregation`. Supported
aggregations are `avg`, `sum`, `min`, `max`, and `count`. `remote_write`
materializes rollup samples into internal collections while preserving the
original label set. `apply_retention_policy` removes expired raw samples from
the metrics collection according to `RETENTION` and leaves rollup data in place.
`query_range` reads the coarsest rollup tier whose target resolution is less
than or equal to the requested `step`; otherwise it reads raw samples.

## AI Provider Path

The AI embedding path emits process-local counters and histograms. Labels never contain request bodies, prompts, API keys, or authorization headers.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_ai_provider_requests_total` | counter | `provider`, `model`, `status` | AI provider requests completed through `AiTransport`. `status` is `ok`, `http_429`, `http_4xx`, `http_5xx`, `http_error`, or `transport_error`. |
| `reddb_ai_provider_request_duration_ms` | histogram | `provider`, `model` | End-to-end provider request duration in milliseconds, including retry wait time. |
| `reddb_ai_provider_retries_total` | counter | `provider`, `reason` | Retry attempts scheduled by `AiTransport`. Reasons include `http_429`, `http_5xx`, `http_error`, and `transport_error`. |
| `reddb_ai_embedding_dedup_hits_total` | counter | - | Embedding dedup cache hits. |
| `reddb_ai_embedding_dedup_misses_total` | counter | - | Embedding dedup cache misses. |
| `reddb_ai_embedding_batch_size` | histogram | `provider` | Distribution of provider sub-batch sizes after deduplication and chunking. |
| `reddb_ai_embedding_chunked_total` | counter | - | Text inputs that exceeded the configured embedding chunk threshold. |
| `reddb_ai_text_tokens_total` | counter | `provider`, `model` | Best-effort token count from embedding provider usage fields, recorded as `prompt_tokens + total_tokens` when present. |

`AiBatchClient` also emits structured developer/audit events without secrets:

| Action | Fields |
|--------|--------|
| `ai/embedding_batch` | `provider`, `model`, `batch_size`, `total_tokens`, `duration_ms`, `retries`, `dedup_hits`, `chunked`, `total_wait_ms`, `prompt_tokens` |
| `ai/embedding_error` | `provider`, `status_code`, `attempt_count`, `total_wait_ms` |

These events are intended for developer signal and audit review. They do not include input text, generated vectors, request JSON, response JSON, URLs, credentials, or headers.
