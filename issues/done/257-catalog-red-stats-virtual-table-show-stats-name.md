# Catalog: red.stats virtual table + SHOW STATS [<name>] [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/257

Labels: enhancement

GitHub issue number: #257

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Virtual table `red.stats` + comando `SHOW STATS [<name>]`. Métricas runtime ortogonais ao schema (qps, hit rate, last write timestamp).

End-to-end:
- `red.stats` schema: `collection`, `entities`, `segments`, `growing_count`, `sealed_count`, `archived_count`, `seal_ops`, `compact_ops`, `last_write_ms`, `attention_score`.
- Materializa de `ManagerStats` + `CatalogModelSnapshot.attention_summary`.
- `SHOW STATS [<name>]` desugar.

## Acceptance criteria

- [ ] `SHOW STATS users` retorna stats de `users`.
- [ ] `SHOW STATS` (sem name) retorna stats de todas Collections.
- [ ] `attention_score` numérico, com valor maior = mais grave.
- [ ] Conformance corpus: 2 casos.
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
