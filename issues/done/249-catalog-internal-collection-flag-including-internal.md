# Catalog: internal collection flag + INCLUDING INTERNAL [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/249

Labels: enhancement

GitHub issue number: #249

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Adiciona `internal: bool` em `CollectionDescriptor` + filtro default em `SHOW COLLECTIONS`.

End-to-end:
- Estende `CollectionDescriptor` com `internal: bool` (default false).
- Novo módulo `InternalCollectionRegistry`: detecta DLQs (criadas via `WITH DLQ`), `audit_log`, auto-policy artifacts. Lista hardcoded inicial.
- `SHOW COLLECTIONS` por default oculta `WHERE internal = false`.
- `SHOW COLLECTIONS INCLUDING INTERNAL` revela tudo.
- Tenant filter ainda aplica (internal não é "público pra todos").

## Acceptance criteria

- [ ] Coluna `internal` aparece em `red.collections`.
- [ ] DLQ criado via `CREATE QUEUE foo WITH DLQ failed_foo` aparece com `internal=true` em `failed_foo`.
- [ ] `SHOW COLLECTIONS` default não retorna `failed_foo`.
- [ ] `SHOW COLLECTIONS INCLUDING INTERNAL` retorna `failed_foo`.
- [ ] Conformance corpus: 2 cases (default hide, including internal).
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
