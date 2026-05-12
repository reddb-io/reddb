# Catalog: red.* schema stability regression test + _experimental_* prefix [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/262

Labels: enhancement

GitHub issue number: #262

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Implementa enforcement automático da stability policy do ADR 0011.

End-to-end:
- Test em CI que falha se uma coluna stable for renamed/removed sem o path de deprecação.
- Suporte ao prefixo `_experimental_*` em `red.*` columns: marca campos voláteis explicitamente.
- Lint test que verifica:
  - Cada coluna stable em `red.*` tem 1+ caso no conformance corpus.
  - Coluna nova adicionada não é breaking (additive).
  - Coluna marcada deprecated emite warning quando queried.
- Doc nova `docs/reference/red-schema.md` enriquecida com tabela: coluna | status (stable/experimental/deprecated) | added_in | deprecated_in.

## Acceptance criteria

- [ ] Test que tenta renomear `entities` → `row_count` em `red.collections` falha CI.
- [ ] Coluna com prefixo `_experimental_*` não está sujeita ao lint.
- [ ] Query em coluna deprecated emite log warning + header `Deprecation`.
- [ ] Doc lista todas colunas com status atual.
- [ ] CI integra o lint na suite padrão.

## Blocked by

- #244
- #254
- #255
- #256
- #257
