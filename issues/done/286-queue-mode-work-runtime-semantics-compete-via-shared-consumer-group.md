# Queue mode: WORK runtime semantics (compete via shared consumer group) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/286

Labels: enhancement

GitHub issue number: #286

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Implementa runtime de `WORK` mode: consumers compartilham um consumer group default implícito, dividindo mensagens (Kafka work-queue pattern).

End-to-end:
- `QUEUE READ foo CONSUMER alice COUNT 10` em queue `WORK`: usa group implícito `_work_default`.
- `QUEUE READ foo CONSUMER bob COUNT 10` (paralelo): também usa `_work_default`. Alice e bob dividem mensagens (não há overlap).
- Backward compat: queues criadas sem mode (todas pre-feature) usam mesmo path.
- Existing `QUEUE GROUP CREATE` ainda funciona pra controle granular — só afeta default routing quando `GROUP` omitido.

## Acceptance criteria

- [ ] 3 consumers em `QUEUE foo WORK` com 100 mensagens: cada consumer recebe ~33 mensagens (não 100).
- [ ] ACK/NACK funciona como hoje.
- [ ] Existing tests de queue passam (backward compat).
- [ ] Conformance: 1 caso pinned WORK semantics.

## Blocked by

- #285
