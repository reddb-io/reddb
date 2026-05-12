# Catalog: red.collections virtual table + SHOW COLLECTIONS [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/244

Labels: enhancement

GitHub issue number: #244

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#239

## What to build

Foundation slice. Estabelece o framework de virtual schema `red.*` + primeiro comando humano `SHOW COLLECTIONS`. Sem isto, nenhuma das 13 slices seguintes funciona.

End-to-end:
- **Engine virtual schema**: módulo novo `runtime/red_schema/` que materializa `red.collections` chamando `catalog::snapshot_store()`. Interface `red_query(virtual_name, filter, projection) -> RowSet`. Read-only — INSERT/UPDATE/DELETE em `red.*` retorna erro "system schema is read-only".
- **Schema inicial de `red.collections`** (additive-only ADR 0011): `name`, `model`, `schema_mode`, `entities`, `segments`, `indices` (count), `in_memory_bytes`, `tenant_id`. (`on_disk_bytes`, `internal`, `attention` virão em slices seguintes.)
- **Parser**: `SHOW COLLECTIONS [WHERE ...]` desugar para `SELECT * FROM red.collections [WHERE ...]`.
- **Auth**: tenant filter mandatório aplicado pelo engine antes de qualquer policy. `cluster:admin` bypass. Read universal dentro do scope.
- **Conformance corpus**: 1 caso pinned `SHOW COLLECTIONS` (TOML em `tests/conformance/`).
- **Doc**: criar `docs/reference/red-schema.md` com schema inicial documentado.

## Acceptance criteria

- [ ] `SELECT * FROM red.collections` retorna lista de Collections do tenant atual via parser SQL existente.
- [ ] `SHOW COLLECTIONS` desugar funciona em todos wires (RedWire, PG, gRPC, HTTP `POST /query`).
- [ ] Tenant `acme` rodando query nunca vê collections de tenant `globex` (test cross-tenant).
- [ ] `cluster:admin` vê todos tenants.
- [ ] `INSERT INTO red.collections VALUES (...)` retorna erro "system schema is read-only".
- [ ] Conformance corpus tem caso `tests/conformance/show_collections.toml` passing.
- [ ] `docs/reference/red-schema.md` lista todas colunas atuais com tipo e descrição.
- [ ] CONTEXT.md já tem termos relacionados (já adicionado nesta sessão).
- [ ] Sem regressão em `cargo test -p reddb-server`.

## Blocked by

None - can start immediately
