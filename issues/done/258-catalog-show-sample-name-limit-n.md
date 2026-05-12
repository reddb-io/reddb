# Catalog: SHOW SAMPLE <name> [LIMIT N] [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/258

Labels: enhancement

GitHub issue number: #258

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Comando `SHOW SAMPLE <name> [LIMIT N]` retorna primeiras N rows da collection. Default N=10. Não passa por `red.*` (é user data, não metadata).

End-to-end:
- Parser desugar para `SELECT * FROM <name> LIMIT N`.
- Default N=10 se não especificado.
- Validação: `<name>` deve ser uma Collection visível no scope (tenant filter via existing path).
- Não suporta WHERE/ORDER BY (intentional — é só sample, não query).
- Random sampling fica fora de scope (MVP retorna primeiras N).

## Acceptance criteria

- [ ] `SHOW SAMPLE users` retorna 10 rows de `users`.
- [ ] `SHOW SAMPLE users LIMIT 5` retorna 5.
- [ ] Tenant filter aplicado (não vê collections de outro tenant).
- [ ] Erro claro se collection não existe ou não é visível.
- [ ] Conformance corpus: 2 casos.

## Blocked by

- #244
