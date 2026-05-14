# `SHOW CREATE TABLE <table>` returns the canonical DDL [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

`SHOW CREATE TABLE <table>` parses today but returns zero rows — the executor stub is missing. Implement the executor so it returns a single-row, single-column result containing a canonical `CREATE TABLE …` string that, when re-executed against a fresh database, produces an equivalent table.

The canonical string includes column names, types, nullability, defaults, and index definitions (as separate `CREATE INDEX` statements if needed).

## Acceptance criteria

- [ ] `SHOW CREATE TABLE <table>` returns one row with one column holding the DDL string.
- [ ] The DDL re-executes against a fresh `.rdb` and produces a table that `DESCRIBE` reports as equivalent to the original.
- [ ] Indexes attached to the table are emitted as separate `CREATE INDEX` statements alongside the main DDL.
- [ ] Unknown table returns a clear `COLLECTION_NOT_FOUND` error.
- [ ] Round-trip integration test: create a table with a non-trivial schema, `SHOW CREATE TABLE`, execute the returned DDL on a new database, `DESCRIBE` both, assert structural equivalence.

## Blocked by

None - can start immediately. Pairs naturally with #473 but is independent.
