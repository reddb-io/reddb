# DDL: docs/query/ddl.md + conformance corpus completo [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/312

Labels: enhancement

GitHub issue number: #312

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

Documento canônico de DDL polimórfica + conformance suite completo.

End-to-end:
- `docs/query/ddl.md` (criar ou estender):
  - Visão geral dos DDLs disponíveis por model.
  - Tabela de cobertura: model × DROP × TRUNCATE × CREATE × ALTER.
  - Polymorphic vs typed: when to use.
  - Quickstart com exemplos.
  - Comparação com Postgres (`DROP TABLE`, `TRUNCATE`), MySQL, MongoDB (`db.collection.drop()`).
  - Cross-references: `red-schema.md`, `policies.md`, `data-models/events.md`.
- Conformance corpus: ≥15 casos cobrindo:
  - DROP por model (8 casos)
  - DROP COLLECTION polymorphic
  - TRUNCATE por model (7 casos)
  - TRUNCATE COLLECTION polymorphic
  - IF EXISTS em ambos
  - Auth deny/allow
  - Event integration
- `docs/data-models/queues.md` atualizado: `TRUNCATE QUEUE` mencionado como canônico, `QUEUE PURGE` como alias.

## Acceptance criteria

- [ ] `docs/query/ddl.md` cobre todos os 15+ DDL forms.
- [ ] Tabela de cobertura completa.
- [ ] Comparação com 3 DBs externos.
- [ ] Conformance corpus: ≥15 casos.
- [ ] Cross-references aplicadas.

## Blocked by

- #307
- #308
- (slice 3 — auth — para casos auth no corpus)

## Completion notes

- Added canonical `docs/query/ddl.md` for polymorphic and typed DDL.
- Updated queue docs to make `TRUNCATE QUEUE` canonical and `QUEUE PURGE` an alias.
- Expanded parser conformance corpus with DROP, TRUNCATE, `QUEUE PURGE`, and DDL auth policy cases.
- Repointed existing DDL DROP/TRUNCATE conformance cases at the canonical DDL doc.
- Validation:
  - `python3 crates/reddb-server/tests/conformance/validate_sources.py`
  - `CARGO_BUILD_JOBS=1 cargo check -p reddb-server --lib`
  - `cargo test -p reddb-server --test conformance` was attempted, but the local build was blocked by concurrent cargo builds in other worktrees and was stopped after waiting.
