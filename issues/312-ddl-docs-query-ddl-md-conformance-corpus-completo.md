# DDL: docs/query/ddl.md + conformance corpus completo [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/312

Labels: needs-triage

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
