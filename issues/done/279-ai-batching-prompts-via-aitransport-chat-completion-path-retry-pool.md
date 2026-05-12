# AI batching: prompts via AiTransport (chat completion path, retry/pool) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/279

Labels: enhancement

GitHub issue number: #279

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Path de prompts (chat completion) reusa `AiTransport` para retry + pool + async + timeouts. Sem batching (prompts geralmente são 1-by-1), mas com mesmo benefício de resiliência.

End-to-end:
- Refator de `ai::openai_prompt` e `ai::anthropic_prompt`:
  - Função pública `prompt(provider, model, request) -> Result<AiPromptResponse>` async.
  - Internamente usa `AiTransport::request`.
  - Retry/backoff em 429/5xx.
  - Connection pool reusada.
- Call sites em ASK pipeline / NER / outros migrados (audit pequeno).
- Mock provider extensão para chat completion responses.
- Integration test: 429 mock → retry → sucesso.

## Acceptance criteria

- [ ] `ai::prompt(provider, model, request)` é async + reusa `AiTransport`.
- [ ] Retry em 429/5xx funcional.
- [ ] Connection pool reusada com embedding paths (mesmo `reqwest::Client`).
- [ ] ASK pipeline migrado para nova API.
- [ ] NER migrado.
- [ ] Mock provider extension cobre chat completion.
- [ ] Integration test 429 → retry observável.
- [ ] Sem regressão em ASK / NER tests existentes.

## Blocked by

- #274
