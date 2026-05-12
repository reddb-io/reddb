# AI batching: HTTP bulk endpoints with AUTO EMBED batch path [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/278

Labels: enhancement

GitHub issue number: #278

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Endpoints HTTP `POST /collections/{name}/bulk/rows` (e variantes nodes/vectors/documents) hoje aceitam array de rows. Adicionar suporte a auto-embed batch quando `auto_embed: { fields: [...], provider: '...' }` está no body.

End-to-end:
- Body extension:
  ```json
  {
    "rows": [{"fields": {...}}, ...],
    "auto_embed": {
      "provider": "openai",
      "fields": ["body"],
      "model": "text-embedding-3-small"
    }
  }
  ```
- Servidor faz `INSERT INTO {name} (...) VALUES (...) WITH AUTO EMBED ...` internamente, reusando path da slice 4.
- Response inclui `embedded_count`, `provider_requests`, `total_tokens` (se disponível).
- Idempotência via `Idempotency-Key` header (já documentado em existing API).

## Acceptance criteria

- [ ] `POST /collections/articles/bulk/rows` com 1000 rows + `auto_embed` → 1 request ao provider mock.
- [ ] Response retorna `created_count`, `embedded_count`, `provider_requests`.
- [ ] Sem `auto_embed` no body → comportamento legacy preservado.
- [ ] Erro provider após retries → 502 ou similar com body explicando; nenhum row inserted.
- [ ] Idempotency-Key respeitada.
- [ ] Documentado em `docs/api/http.md` + `docs/query/insert.md`.

## Blocked by

- #276
