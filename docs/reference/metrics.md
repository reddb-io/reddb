# Metrics Reference

RedDB exposes Prometheus text format at `GET /metrics`.

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
