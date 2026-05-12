# Events: operations filter + WHERE filter [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/297

Labels: enhancement

GitHub issue number: #297

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Permite filtrar quais operações + quais rows disparam eventos.

End-to-end:
- `WITH EVENTS (INSERT, UPDATE)` — DELETE não dispara.
- `WITH EVENTS WHERE status = 'active'` — só rows que matched o predicate disparam.
- WHERE evaluado em pre-commit (UPDATE/DELETE: avalia `after`-state; INSERT: avalia row inserida).

## Acceptance criteria

- [ ] `WITH EVENTS (INSERT)` ignora UPDATEs e DELETEs.
- [ ] `WITH EVENTS WHERE status = 'active'`: rows com `status = 'inactive'` não geram evento.
- [ ] Combinação `(INSERT, UPDATE) WHERE x` funcional.
- [ ] Conformance: 4 casos.

## Blocked by

- #292
