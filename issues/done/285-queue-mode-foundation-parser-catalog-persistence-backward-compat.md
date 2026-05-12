# Queue mode: foundation parser + catalog persistence + backward compat [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/285

Labels: enhancement

GitHub issue number: #285

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Foundation. Adiciona modo `FANOUT|WORK` como flag first-class na DDL de queue + persiste no catalog. Sem mudança de runtime nesta slice (queues continuam comportando como hoje).

End-to-end:
- Parser: `CREATE QUEUE foo [FANOUT|WORK]` + `ALTER QUEUE foo SET MODE [FANOUT|WORK]`.
- AST: `QueueMode` enum em `storage/queue/mode.rs`.
- Catalog: `QueueDescriptor.mode: QueueMode` persistido (additive, ADR 0011).
- Backward compat: queues criadas pré-feature default `Work` ao ler do catalog.
- 1 conformance case + parser test.
- Doc preview no `docs/data-models/queues.md` (placeholder pra slice 6).

## Acceptance criteria

- [ ] `CREATE QUEUE foo FANOUT` parse OK + persiste mode `Fanout` no catalog.
- [ ] `CREATE QUEUE foo WORK` parse OK + persiste `Work`.
- [ ] `CREATE QUEUE foo` (sem mode) → default `Work`.
- [ ] `ALTER QUEUE foo SET MODE FANOUT` parse OK + atualiza catalog.
- [ ] Catalog reload: queue criada pré-feature lê como `Work`.
- [ ] `red.collections.queue_mode` column populado (mas slice 5 expõe oficialmente).
- [ ] Conformance corpus: 4 casos (FANOUT, WORK, default, ALTER).
- [ ] Sem mudança em comportamento runtime ainda — queues entregam mensagens como hoje.

## Blocked by

None - can start immediately
