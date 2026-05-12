# Catalog: typed SHOW filters (TABLES/QUEUES/VECTORS/DOCUMENTS/TIMESERIES/GRAPHS/KV) [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/248

Labels: enhancement

GitHub issue number: #248

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

7 comandos filtrados sobre `red.collections`:

- `SHOW TABLES` → `... WHERE model = 'table'`
- `SHOW QUEUES` → `... WHERE model = 'queue'`
- `SHOW VECTORS` → `... WHERE model = 'vector'`
- `SHOW DOCUMENTS` → `... WHERE model = 'document'`
- `SHOW TIMESERIES` → `... WHERE model = 'timeseries'`
- `SHOW GRAPHS` → `... WHERE model = 'graph'`
- `SHOW KV` → `... WHERE model = 'kv'`

Cada um é macro de parser; reusa execução de `SHOW COLLECTIONS`.

## Acceptance criteria

- [ ] 7 comandos parseiam e executam corretamente.
- [ ] Tenant filter aplicado (cross-tenant test).
- [ ] Conformance corpus tem 7 casos pinned (`show_tables.toml`, `show_queues.toml`, etc).
- [ ] Doc atualizado em `docs/reference/red-schema.md` mostrando os 7 atalhos.

## Blocked by

- #244
