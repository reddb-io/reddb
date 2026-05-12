# Docs: docs/reference/red-schema.md canonical reference [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/263

Labels: enhancement

GitHub issue number: #263

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Documento canônico de referência para o `red.*` schema. Source of truth para cada virtual table + cada coluna.

Estrutura:
- Visão geral do schema `red.*` (papel, stability policy referenciando ADR 0011).
- Tabela por virtual table: `red.collections`, `red.columns`, `red.indices`, `red.policies`, `red.stats`.
- Cada virtual table: lista de colunas com nome, tipo, descrição, status (stable/experimental/deprecated), since version.
- Exemplos de queries comuns por caso de uso (capacity planning, audit, debug).
- Cross-reference para ADR 0010 (PG translator) e ADR 0011 (stability).
- Política de evolução documentada inline.

## Acceptance criteria

- [ ] `docs/reference/red-schema.md` criado.
- [ ] Cada virtual table documentada com tabela de colunas.
- [ ] Cada coluna tem status, since version, descrição.
- [ ] 5+ exemplos de query por caso de uso.
- [ ] Links para ADR 0010, 0011.
- [ ] Doc cited em `docs/README.md` ou index principal.

## Blocked by

- #244
- #254
- #255
- #256
- #257
