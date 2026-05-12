# Catalog: PG-wire translator (pg_class/namespace/tables/attribute/index/constraint/database) [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/260

Labels: enhancement

GitHub issue number: #260

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Novo módulo `wire/postgres/translator.rs` que reescreve queries `pg_*` para `red.*`. Conforme ADR 0010 — engine nunca vê conceitos PG.

7 tabelas suportadas (mínimo para Prisma/SQLAlchemy/Hibernate/Metabase/DBeaver):
- `pg_class` → `red.collections` (com mapping de `relkind`)
- `pg_namespace` → schema names
- `pg_tables` → filtro `WHERE relkind='r'`
- `pg_attribute` → `red.columns`
- `pg_index` → `red.indices`
- `pg_constraint` → `red.columns` (where is_primary_key/is_unique)
- `pg_database` → fixed single response com nome do banco

End-to-end:
- Detecção de queries que tocam `pg_*` no listener PG (antes do engine).
- Reescrita textual + AST-based para queries comuns.
- Caso não-suportado: passa pro engine (que retorna table-not-found) com log warning.
- Conformance: queries reais dos drivers (capturadas via tcpdump ou docs oficiais Prisma/SQLAlchemy).

## Acceptance criteria

- [ ] `psql` conectando via PG-wire e rodando `\dt` mostra todas tables visíveis no scope.
- [ ] Prisma probe query (`SELECT relname FROM pg_class WHERE relnamespace = ...`) retorna lista non-empty.
- [ ] Hibernate metadata query funcional.
- [ ] DBeaver reconhece schema na conexão.
- [ ] `SELECT * FROM red.collections` direto via PG-wire funciona (bypass tradução).
- [ ] Tradução nunca alcança RedWire/gRPC/HTTP wires (só PG).
- [ ] Conformance corpus: 7 casos PG (1 por tabela).
- [ ] Doc novo: `docs/architecture/wire-adapters.md` com lista das 7 traduções.

## Blocked by

- #244
- #254
- #255
