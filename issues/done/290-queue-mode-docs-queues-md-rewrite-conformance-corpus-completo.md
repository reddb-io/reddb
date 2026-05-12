# Queue mode: docs queues.md rewrite + conformance corpus completo [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/290

Labels: enhancement

GitHub issue number: #290

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Reescreve `docs/data-models/queues.md` com FANOUT/WORK como conceito primário; consumer groups vira power-user content.

End-to-end:
- Estrutura nova em `docs/data-models/queues.md`:
  - Quick start: `CREATE QUEUE notifications FANOUT` + `CREATE QUEUE tasks WORK`
  - Use cases por mode (broadcast vs work)
  - Tabela comparativa: FANOUT/WORK vs RabbitMQ/Pulsar/Kafka
  - Advanced: consumer groups granulares (mantém docs hoje)
  - ALTER mode + semantics
- Conformance corpus completo: cada DDL form + ALTER + edge cases.
- Cross-reference em `docs/reference/red-schema.md`.

## Acceptance criteria

- [ ] `docs/data-models/queues.md` reescrito com FANOUT/WORK como primário.
- [ ] Tabela comparativa com 3 sistemas externos.
- [ ] Quickstart cobre os 2 modos com exemplo real.
- [ ] Conformance corpus: ≥10 casos cobrindo CREATE, ALTER, edge cases.
- [ ] Cross-reference em red-schema.md.

## Blocked by

- #286
- #287
