# Events: UPDATE / DELETE events [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/293

Labels: needs-triage

GitHub issue number: #293

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Estende emissão de eventos para UPDATE e DELETE.

End-to-end:
- **UPDATE**: payload com `before` (estado pré-mutação) + `after` (pós-mutação). Apenas campos alterados em `before`/`after` (otimização: não envia row inteira se só 1 campo mudou).
- **DELETE**: payload com `before` cheio + `after: null`.
- Multi-row UPDATE/DELETE: N eventos em ordem.
- Integration tests cobrem ambos.

## Acceptance criteria

- [ ] `UPDATE users SET name = 'X' WHERE id = 42` produz evento `{op: "update", before: {name: "Alice"}, after: {name: "X"}}`.
- [ ] `DELETE FROM users WHERE id = 42` produz evento `{op: "delete", before: {id:42, name:"X", ...}, after: null}`.
- [ ] Multi-row UPDATE: N eventos.
- [ ] Optimization: UPDATE de 1 campo em row de 50 colunas não envia 50 fields desnecessários.
- [ ] Conformance: 4 casos (UPDATE single, UPDATE multi, DELETE single, DELETE multi).

## Blocked by

- #292
