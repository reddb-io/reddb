# Column policy: wire enforcement em SELECT relacional (table.rs) [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/265

Labels: enhancement

GitHub issue number: #265

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#240

## What to build

Wire `ColumnPolicyGate::gate(principal, Select, projected_columns)` em `runtime/query_exec/table.rs`. Path mais frequente.

End-to-end:
- Antes de retornar rows do SELECT relacional, chama gate com colunas projetadas.
- Colunas negadas viram `null` no result set (configurável: erro 403 com policy condition `enforce_strict: true`).
- Suporta wildcard SELECT `*`: gate verifica todas colunas; negadas saem do resultset.
- Suporta SELECT com aliases.
- Conformance + integration tests.

## Acceptance criteria

- [ ] Policy `DENY select ON column:users.email` faz `SELECT email FROM users` retornar `null`.
- [ ] `SELECT * FROM users` esconde `email` (não aparece no resultset) ou retorna null (decidir nesta slice).
- [ ] `SELECT name, email AS e FROM users` honra alias.
- [ ] Wildcard `column:*.email` aplica.
- [ ] Test: policy = `enforce_strict: true` retorna 403 em vez de null.
- [ ] Conformance corpus: 5+ casos.

## Blocked by

- #264
