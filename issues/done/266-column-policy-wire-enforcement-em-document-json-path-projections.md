# Column policy: wire enforcement em document JSON path projections [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/266

Labels: enhancement

GitHub issue number: #266

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#240

## What to build

Wire column gate em queries sobre documents que usam JSON path (`body->>'email'`).

End-to-end:
- Path `body.email` (e nested `body.profile.phone`) mapeia para `column:<collection>.body.email`.
- Path normalizer no executor de document.
- Gate aplicado antes de retornar valor.
- Negado retorna `null` no result.

## Acceptance criteria

- [ ] Policy `DENY select ON column:logs.body.password` faz `SELECT body->>'password' FROM logs` retornar `null`.
- [ ] Nested path: `body.user.email` → `column:logs.body.user.email`.
- [ ] Wildcard suportado: `column:*.body.password`.
- [ ] Conformance corpus: 4+ casos (top-level field, nested, wildcard, multiple denies).

## Blocked by

- #264
