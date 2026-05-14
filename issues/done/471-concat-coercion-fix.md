# Fix `||` (column + literal) and `CONCAT()` returning re-quoted values [AFK]

Labels: bug, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

`'a' || 'b'` returns `'ab'` correctly today (literal-only concat works). But:

- `SELECT name || '!' FROM t LIMIT 1` returns `{"CONCAT":"'updated''!'"}` — the column value and the literal are re-quoted at concat time.
- `SELECT CONCAT('a', 'b')` returns `{"CONCAT":"'a''b'"}` — the function form takes a different render branch that always re-quotes.

Unify both paths through a single string-coercion helper that converts any `Value` variant to its plain text form (no SQL-literal escaping) before concatenation. The helper is the deep module: one entry point, easy to property-test.

## Acceptance criteria

- [ ] `SELECT 'a' || 'b'` returns `"ab"` (already works; covered as regression).
- [ ] `SELECT name || '!' FROM t LIMIT 1` (column + literal) returns `"<name>!"` with no surrounding quotes.
- [ ] `SELECT name || other FROM t` (column + column) returns the concatenated text values.
- [ ] `SELECT CONCAT('a', 'b')` returns `"ab"`.
- [ ] `SELECT CONCAT(name, '!')` returns the same as `name || '!'`.
- [ ] Mixed-type concat (e.g. `name || ' (' || id || ')'`) coerces non-text variants via the helper and returns a plain string.
- [ ] Property test on the coercion helper: every `Value` variant has a deterministic plain-text form.
- [ ] Integration tests for each of the bullets above.

## Blocked by

None - can start immediately.
