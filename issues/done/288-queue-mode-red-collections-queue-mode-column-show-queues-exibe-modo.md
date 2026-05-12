# Queue mode: red.collections.queue_mode column + SHOW QUEUES exibe modo [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/288

Labels: enhancement

GitHub issue number: #288

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Expõe `queue_mode` em introspection para que operadores enxerguem o modo de cada queue.

End-to-end:
- `red.collections` ganha coluna `queue_mode: Option<text>` (FANOUT|WORK|null para non-queues). Additive ADR 0011.
- `SHOW QUEUES` (filtro tipado de PRD #239) inclui `queue_mode` no output default.
- `SHOW COLLECTIONS` mostra `queue_mode` em VERBOSE; oculto em default (só queues têm valor).
- Doc atualizado em `docs/reference/red-schema.md`.

## Acceptance criteria

- [ ] `SELECT name, queue_mode FROM red.collections WHERE model = 'queue'` retorna FANOUT/WORK por queue.
- [ ] `SHOW QUEUES` exibe `queue_mode` como coluna.
- [ ] `red.collections.queue_mode = NULL` para tables/documents/etc.
- [ ] Conformance: 2 casos (queue FANOUT visível, queue WORK visível).
- [ ] Doc atualizado.

## Blocked by

- #285
