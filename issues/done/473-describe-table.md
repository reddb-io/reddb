# `DESCRIBE <table>` returns one row per column [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

Today `DESCRIBE tt` produces `unable to detect query mode`. Without `DESCRIBE`, the only way to learn a table's schema is to `SELECT *` and inspect the keys — which is also noisy because of the `red_*` columns until PRD #445 / issue #450 lands.

Add `DESCRIBE <table>` as a recognized statement. Returns one row per user-declared column with `(name, type, nullable, default, indexed)`.

## Acceptance criteria

- [ ] `DESCRIBE <table>` parses and is routed to the SQL executor (not SPARQL / Cypher / Natural).
- [ ] Returns one row per user-declared column.
- [ ] Row shape: `(name, type, nullable, default, indexed)`. `indexed` is true when at least one index includes the column.
- [ ] Internal `red_*` columns are NOT listed.
- [ ] `DESCRIBE <unknown_table>` returns a clear `COLLECTION_NOT_FOUND` error.
- [ ] Works for table, graph, vector, timeseries collections (or returns a clear `NOT_APPLICABLE` for kinds without per-column schemas).
- [ ] Integration test: create a table with a typed column set, add one index, assert `DESCRIBE` returns the expected rows.

## Blocked by

None - can start immediately.
