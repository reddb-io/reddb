# Docs sweep: parameterized form as default in vectors/query/driver guides [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/374

Labels: needs-triage

GitHub issue number: #374

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Sweep the docs so the parameterized form is the default shown to new users. Replace string-concat / `JSON.stringify` examples with `db.query(sql, params)`.

Pages in scope:

- `docs/data-models/vectors.md` — every SEARCH SIMILAR / INSERT example
- `docs/vectors/hnsw.md`, `docs/vectors/ivf.md`
- `docs/query/select.md`, `docs/query/insert.md`, `docs/query/update.md`, `docs/query/delete.md`, `docs/query/search-commands.md`, `docs/query/universal.md`
- `docs/guides/javascript-typescript-driver.md` and equivalent driver guides
- `docs/api/embedded.md`, `docs/api/http.md`, `docs/api/postgres-wire.md`
- `docs/getting-started/quick-start.md`

Add a "Safe parameter binding" section to the JS/TS driver guide showing the vector example.

## Acceptance criteria

- [ ] All listed docs updated.
- [ ] No vector or user-input example in docs uses string concatenation any longer.
- [ ] Each driver guide shows the parameterized form as the first example.
- [ ] Cross-link to the ADR (#352) from the relevant driver and query pages.

## Blocked by

- #361
- #362
- #363
