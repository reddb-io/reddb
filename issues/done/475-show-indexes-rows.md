# `SHOW INDEXES` / `SHOW INDICES` returns one row per index [AFK]

Labels: bug, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

After `CREATE INDEX`, `SHOW INDEXES` returns an empty result today. The indexes themselves work (verified via `EXPLAIN` showing `index_seek`) — the catalog read path just doesn't surface them.

Two fixes in one slice:

- Accept both `SHOW INDEXES` and `SHOW INDICES` in the mode detector / parser (today only `INDICES` is recognized as a prefix).
- Populate the executor so it returns one row per index with `(name, table, columns, kind, unique, entries_indexed)`.

## Acceptance criteria

- [ ] `SHOW INDEXES` parses and returns rows.
- [ ] `SHOW INDICES` parses and returns the same rows.
- [ ] Row shape: `(name, table, columns, kind, unique, entries_indexed)`.
- [ ] `kind` distinguishes `BTREE` from any future variants.
- [ ] `unique` is true for indexes created with `CREATE UNIQUE INDEX`.
- [ ] `columns` is the ordered list of indexed columns (string or array per the result codec convention).
- [ ] Integration test: create one regular and one unique index, `SHOW INDEXES` returns both with correct attributes.
- [ ] `EXPLAIN`'s `index_seek` continues to fire on the same indexes after this change.

## Blocked by

None - can start immediately.
