# Catalog: red.columns virtual table + SHOW SCHEMA <name> [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/254

Labels: enhancement

GitHub issue number: #254

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Virtual table `red.columns` + comando `SHOW SCHEMA <name>` que mostra colunas, types, constraints, defaults de uma collection.

End-to-end:
- `red.columns` schema: `collection`, `name`, `type`, `nullable`, `default_value`, `is_primary_key`, `is_unique`.
- Materializa de `CollectionDescriptor.schema` (que tem column metadata).
- `SHOW SCHEMA <name>` desugar para `SELECT * FROM red.columns WHERE collection = '<name>'`.
- Funciona para todos os models que têm schema (`table`, `document` strict, `timeseries`, `kv`). Para `queue`/`graph`/`vector` retorna empty ou sintetiza estrutura conhecida.

## Acceptance criteria

- [ ] `SELECT * FROM red.columns WHERE collection = 'users'` retorna colunas de `users`.
- [ ] `SHOW SCHEMA users` produz mesma saída.
- [ ] Document collections com schema inferido aparecem com colunas top-level + nullable.
- [ ] Conformance corpus: 3 casos (table, document, schemaless).
- [ ] Doc atualizado em `docs/reference/red-schema.md`.

## Blocked by

- #244
