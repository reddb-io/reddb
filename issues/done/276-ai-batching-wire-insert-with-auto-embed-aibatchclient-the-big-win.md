# AI batching: wire INSERT WITH AUTO EMBED → AiBatchClient (THE BIG WIN) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/276

Labels: enhancement

GitHub issue number: #276

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

**A slice que entrega o speedup.** Substitui o loop per-row em `runtime/impl_dml.rs:608-632` pelo padrão collector → batch → distribute.

End-to-end:
- Refator do path AUTO EMBED em `runtime/impl_dml.rs`:
  - **Collector phase**: itera `recent` entities, extrai textos (combined dos fields configurados), monta `Vec<(EntityId, String)>`.
  - **Batch phase**: filtra textos não-vazios, chama `AiBatchClient::embed_batch(provider, model, texts)`.
  - **Distribute phase**: usa o `entity_id` correspondente a cada índice para fazer `create_vector` com o embedding correto.
- Bench validação: rodar bench harness (slice 1) antes/depois com mock provider 100ms latency. Alvo: ≥50× speedup (1000 rows: 100s → ≤2s).
- Integration test: `INSERT 1000 rows WITH AUTO EMBED USING mock_provider` faz **exatamente 1 request** ao mock (ou ⌈1000/max_batch⌉ requests).
- Backward compat: `WITH AUTO EMBED` sintaxe inalterada; comportamento externo idêntico.

## Acceptance criteria

- [ ] `INSERT … VALUES (..1000 rows..) WITH AUTO EMBED USING mock` faz `mock.total_requests == ⌈1000/max_batch⌉`.
- [ ] Para max_batch=2048: exatamente 1 request, com 1000 inputs.
- [ ] Bench mostra ≥50× speedup vs baseline da slice 1 (mock latency 100ms).
- [ ] Row com texto vazio skipped sem abortar restantes.
- [ ] Erro provider após retries → INSERT abortado com erro claro, **nenhum** vector_id criado.
- [ ] Existing AUTO EMBED tests continuam passing.
- [ ] Documentado em release notes: speedup mensurado.

## Blocked by

- #273
- #275
