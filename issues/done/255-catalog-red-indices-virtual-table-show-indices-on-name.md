# Catalog: red.indices virtual table + SHOW INDICES ON <name> [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/255

Labels: enhancement

GitHub issue number: #255

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Virtual table `red.indices` + comando `SHOW INDICES ON <name>`.

End-to-end:
- `red.indices` schema: `collection`, `name`, `kind`, `enabled`, `build_state`, `queryable`, `requires_rebuild`.
- Materializa de `CatalogModelSnapshot.index_statuses` + `operational_indexes`.
- `SHOW INDICES [ON <name>]` desugar para `SELECT * FROM red.indices [WHERE collection = '<name>']`.

## Acceptance criteria

- [ ] `SELECT * FROM red.indices` lista todos índices visíveis ao tenant.
- [ ] `SHOW INDICES ON users` filtra por collection.
- [ ] `SHOW INDICES` (sem ON) lista todos.
- [ ] Inclui status: declared/operational/in_sync/queryable.
- [ ] Conformance corpus: 2 casos (geral + filtered by collection).
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
