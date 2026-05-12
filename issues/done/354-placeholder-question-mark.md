# Add ? positional placeholder syntax to parser [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/354

Labels: enhancement

GitHub issue number: #354

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Extend the placeholder parser to accept `?` positional placeholders in addition to `$N`. Mixing both in a single statement is rejected. With `?`, placeholders are numbered left-to-right starting at 1 and map to the same parameter slot model.

The binder needs no changes — it operates on the slot-indexed AST that the parser already produces. JS SDK exercises both forms in tests.

## Acceptance criteria

- [ ] `SELECT * FROM users WHERE id = ? AND name = ?` parses and binds correctly.
- [ ] Mixing `?` and `$N` in one statement returns a clear parse error.
- [ ] `?` inside string literals and comments is ignored.
- [ ] Tests cover: well-formed `?...?`, mixed (rejected), `?` inside literals (ignored), arity mismatch.

## Blocked by

- #353
