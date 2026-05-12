# AI batching: metrics Prometheus + structured audit log [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/280

Labels: enhancement

GitHub issue number: #280

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Observabilidade completa do path AI: métricas Prometheus + logs estruturados.

Métricas (Prometheus, expostas em `/metrics`):
- `reddb_ai_provider_requests_total{provider,model,status}` — counter
- `reddb_ai_provider_request_duration_ms{provider,model}` — histogram
- `reddb_ai_provider_retries_total{provider,reason}` — counter
- `reddb_ai_embedding_dedup_hits_total` — counter
- `reddb_ai_embedding_dedup_misses_total` — counter
- `reddb_ai_embedding_batch_size{provider}` — histogram
- `reddb_ai_embedding_chunked_total` — counter
- `reddb_ai_text_tokens_total{provider,model}` — counter (best-effort: prompt_tokens + total_tokens da response)

Audit log estruturado (via existing logger):
- Por batch: `provider`, `model`, `batch_size`, `total_tokens`, `duration_ms`, `retries`, `dedup_hits`, `chunked`.
- Por erro: `provider`, `status_code`, `attempt_count`, `total_wait_ms`.

End-to-end:
- Hooks em `AiTransport` + `AiBatchClient` + `EmbeddingDedupCache`.
- Métricas publicadas via existing Prometheus endpoint.
- Logs no telemetry channel apropriado (slow_query ou developer signal).
- Documentado em `docs/reference/metrics.md` (criar ou estender).

## Acceptance criteria

- [ ] `/metrics` retorna todas métricas listadas com valores corretos após 1 INSERT WITH AUTO EMBED.
- [ ] Batch size histogram populado.
- [ ] Retry counter incrementado quando mock retorna 429.
- [ ] Dedup hits counter incrementado quando dedup ON e cache hits.
- [ ] Audit log line por batch com fields completos (smoke test em fixture).
- [ ] Documentado em `docs/reference/metrics.md`.

## Blocked by

- #275
