# Queue mode: ALTER QUEUE SET MODE + warning on active consumers [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/289

Labels: needs-triage

GitHub issue number: #289

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Implementa `ALTER QUEUE foo SET MODE FANOUT|WORK` em runtime. Mensagens em flight ficam com mode antigo; novas usam mode novo.

End-to-end:
- DDL apply: atualiza catalog + invalida cache de queue.
- Detecção de consumers ativos (`pending` count > 0): emite warning operacional sem bloquear.
- Nova subscription/READ usa mode atualizado.
- Documenta semantics: "in-flight messages drained with old mode; new pushes use new mode".

## Acceptance criteria

- [ ] `ALTER QUEUE foo SET MODE FANOUT` em queue WORK funciona, novos READ usam FANOUT.
- [ ] Consumers ativos com pending messages: warning emitido (audit log + tracing).
- [ ] In-flight messages drained com mode antigo (não migram).
- [ ] Conformance: 1 caso transição WORK→FANOUT.
- [ ] Doc atualizado em queues.md.

## Blocked by

- #286
- #287
