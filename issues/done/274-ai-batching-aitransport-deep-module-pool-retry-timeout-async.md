# AI batching: AiTransport deep module (pool + retry + timeout + async) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/274

Labels: enhancement

GitHub issue number: #274

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Módulo central de transporte HTTP para AI providers. Substitui calls síncronas/blocking + sem retry pelo `reqwest::Client` compartilhado com pool, timeout, retry/backoff exponencial.

End-to-end:
- Novo módulo `runtime/ai/transport.rs` com `AiTransport`:
  - Interface única `request(builder: RequestBuilder) -> Result<Response>`.
  - `reqwest::Client` global compartilhado (pool de 32 conexões/host por default).
  - Async (`async fn`).
  - Timeout default 30s, configurável via `runtime.ai.transport_timeout_ms`.
  - Retry exponential backoff em 429/5xx/timeout/connection-refused: default 3 tries, base 500ms × 2^n, cap 10s.
  - Erro propagado após retry exhausted: `provider`, `status_code`, `attempt_count`, `total_wait_ms`.
- Integration tests usando mock provider (slice 1, ou stub temporário se 1 ainda não landou).
- Bench: 100 paralelos requests reusam ≤32 conexões TCP (asserto via mock counter).

## Acceptance criteria

- [ ] `AiTransport::request` é async e não bloqueia thread.
- [ ] Connection pool reusa conexões: 100 calls reusam ≤32 sockets (mock socket counter assert).
- [ ] 429 → retry com backoff observável; sucesso na 3ª chamada → request total succeeds.
- [ ] 5xx → mesma política.
- [ ] Após 3 retries falhadas → erro contextualizado com `attempt_count`, `total_wait_ms`.
- [ ] Timeout default 30s atinge → erro `RequestTimeout`.
- [ ] Configs: `runtime.ai.transport_pool_size`, `_timeout_ms`, `_retry_max_attempts`, `_retry_base_ms`.
- [ ] Sem regressão em testes existentes que usam `openai_*` direto.

## Blocked by

None - can start immediately
