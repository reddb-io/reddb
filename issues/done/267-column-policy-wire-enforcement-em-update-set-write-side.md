# Column policy: wire enforcement em UPDATE SET (write-side) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/267

Labels: enhancement

GitHub issue number: #267

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#240

## What to build

Wire column gate em UPDATE para bloquear escrita em colunas sensíveis.

End-to-end:
- `UPDATE users SET email = ...` chama `gate(principal, Update, ["users.email"])`.
- Coluna negada: write rejeitado com erro 403 OU silenciosamente ignored (decisão por slice; recomendo erro 403 — diferente de SELECT).
- Multi-column UPDATE: SET col1 = ..., col2 = ... — ambas verificadas antes de aplicar.
- Conformance + integration tests.

## Acceptance criteria

- [ ] Policy `DENY update ON column:users.email` rejeita `UPDATE users SET email = ...` com 403.
- [ ] Multi-column: `UPDATE users SET name = X, email = Y` rejeita ambas mesmo se name é OK (atomic).
- [ ] Sem policy: comportamento atual preservado.
- [ ] Conformance corpus: 4+ casos.

## Blocked by

- #264
