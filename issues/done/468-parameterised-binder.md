# Parameterised binder: SQL `?` / `?N` placeholders bind params from JSON-RPC envelope [AFK]

Labels: enhancement, security, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

With the mode detector fix in #467, SQL queries with `?` placeholders reach the SQL parser. Wire the parameterised binder so that `db.query(sql, params)` works end-to-end across SELECT / INSERT / UPDATE / DELETE.

The SDK already types `db.query(sql, params: Array<…>)`. The JSON-RPC envelope already carries a `params` field. Discover placeholders in the parsed AST, coerce each JSON value to the schema `Value` variant of its target slot, and inject the values into the AST before lowering. Today every embedded-mode caller string-interpolates manually — fixing this closes a real injection footgun.

## Acceptance criteria

- [ ] `db.query("SELECT name FROM t WHERE id = ?", [5])` returns the row.
- [ ] Same shape works for `INSERT`, `UPDATE`, `DELETE`.
- [ ] Type coercion: JSON `number` → integer/float, `string` → text, `null` → NULL, JSON array → vector (where the slot is a vector).
- [ ] Wrong-arity (too few or too many `params`) produces a clear error.
- [ ] Type mismatch (e.g. string passed where integer is expected) produces a clear error naming the slot and the expected type.
- [ ] Single-quote in a string param is bound safely (no SQL escaping required from the caller).
- [ ] Property test: a randomly-generated SQL with N placeholders, each bound to a random JSON value within the slot's allowed types, executes equivalently to its inlined-literal counterpart.
- [ ] Integration tests via stdio JSON-RPC covering all four DML statement kinds.

## Blocked by

- #467 (mode detector must stop routing `?` to SPARQL first)
