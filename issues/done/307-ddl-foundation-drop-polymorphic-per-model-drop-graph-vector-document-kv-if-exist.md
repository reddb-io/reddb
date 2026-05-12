# DDL: foundation — DROP polymorphic + per-model DROP (graph/vector/document/kv) + IF EXISTS [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/307

Labels: enhancement

GitHub issue number: #307

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

Foundation slice — fecha cobertura de DROP em todos os 7 models + adiciona polymorphic `DROP COLLECTION`. Sem isto, slices subsequentes não funcionam.

End-to-end:
- Parser: adiciona `DROP GRAPH`, `DROP VECTOR`, `DROP DOCUMENT`, `DROP KV` + `DROP COLLECTION` (polymorphic).
- AST core: `QueryExpr::DropGraph/DropVector/DropDocument/DropKv/DropCollection` com `(name, if_exists)`.
- Polymorphic resolver (deep module novo `runtime/ddl/polymorphic_resolver.rs`): `resolve(name, scope) -> Result<CollectionModel>`. Lookup catalog snapshot.
- Executor:
  - `DROP <TYPED> foo` valida model match contra resolver. Mismatch → erro "expected <type>, got <actual>".
  - `DROP COLLECTION foo` → resolver dispatch para handler typed correto.
- `IF EXISTS` em todas as variantes — sem erro se collection não existe.
- Conformance corpus: ≥6 casos pinned.

## Acceptance criteria

- [ ] `DROP GRAPH identity` parse + remove collection com model graph.
- [ ] `DROP VECTOR notes`, `DROP DOCUMENT logs`, `DROP KV settings` análogos.
- [ ] `DROP COLLECTION users` polymorphic — funciona independente do model.
- [ ] `DROP TABLE foo` em queue → erro "model mismatch: expected table, got queue".
- [ ] `DROP TABLE IF EXISTS nonexistent` → succeed silently.
- [ ] `DROP COLLECTION IF EXISTS nonexistent` → succeed silently.
- [ ] DROP em `red.*` → erro "system schema is read-only".
- [ ] Conformance corpus: cobertos os 6 forms novos + IF EXISTS.
- [ ] Sem regressão em DROP TABLE / DROP QUEUE / DROP TIMESERIES existentes.

## Blocked by

- #244
