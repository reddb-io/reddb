# Queue mode: FANOUT runtime semantics (broadcast via implicit group-per-consumer) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/287

Labels: enhancement

GitHub issue number: #287

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#283

## What to build

Implementa runtime de `FANOUT` mode: cada consumer name vira um consumer group implícito separado, recebendo todas as mensagens.

End-to-end:
- `QUEUE READ foo CONSUMER alice COUNT 10` em queue `FANOUT`: usa group implícito `_fanout_alice`.
- `QUEUE READ foo CONSUMER bob COUNT 10`: usa `_fanout_bob`.
- Alice e bob recebem **todas** as mensagens (cada um tem seu próprio offset).
- Cada consumer tem ack/offset isolado.

## Acceptance criteria

- [ ] 3 consumers em `QUEUE foo FANOUT` com 100 mensagens: cada consumer recebe **100** mensagens.
- [ ] Cada consumer mantém seu offset isolado.
- [ ] ACK em alice não afeta bob.
- [ ] DLQ por consumer (alice falhada não afeta bob).
- [ ] Conformance: 1 caso pinned FANOUT semantics.

## Blocked by

- #285
