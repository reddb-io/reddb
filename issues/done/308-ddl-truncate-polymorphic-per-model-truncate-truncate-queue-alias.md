# DDL: TRUNCATE polymorphic + per-model TRUNCATE + TRUNCATE QUEUE alias [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/308

Labels: enhancement

GitHub issue number: #308

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

TRUNCATE para todos models. Reusa polymorphic resolver da slice 1.

End-to-end:
- Parser: adiciona token `TRUNCATE` como statement keyword (já existe como GRANT name; agora é comando).
- DDL forms: `TRUNCATE TABLE foo`, `TRUNCATE GRAPH foo`, `TRUNCATE VECTOR foo`, `TRUNCATE DOCUMENT foo`, `TRUNCATE TIMESERIES foo`, `TRUNCATE KV foo`, `TRUNCATE QUEUE foo`, `TRUNCATE COLLECTION foo`.
- AST: `QueryExpr::Truncate(TruncateQuery { name, model: Option<CollectionModel>, if_exists })`. `model = None` = polymorphic.
- Executor:
  - Per-model: rows zeradas, schema preservado. Native impl por model (table: WAL log + B-tree clear; queue: alias para `QUEUE PURGE` impl existente; vector: clear vector index + entity table; etc).
  - `TRUNCATE QUEUE = QUEUE PURGE` — mesmo executor, vocabulário consistente.
  - Polymorphic: resolver → dispatch.
- IF EXISTS em todas variantes.
- Conformance corpus.

## Acceptance criteria

- [ ] `TRUNCATE TABLE users` → todas rows removidas, schema/índices preservados.
- [ ] `TRUNCATE QUEUE tasks` → alias para `QUEUE PURGE tasks`. Mesmo estado externo.
- [ ] `TRUNCATE COLLECTION foo` polymorphic.
- [ ] `TRUNCATE GRAPH/VECTOR/DOCUMENT/TIMESERIES/KV` funcionam por model.
- [ ] Model mismatch (`TRUNCATE TABLE foo` em queue) → erro.
- [ ] `IF EXISTS` em todas.
- [ ] TRUNCATE em `red.*` → erro.
- [ ] Conformance corpus: ≥8 casos.
- [ ] `QUEUE PURGE` existing tests passam (backward compat).

## Blocked by

- #307
