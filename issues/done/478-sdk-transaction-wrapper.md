# SDK `db.transaction(async (tx) => {...})` wrapper [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

Add a `db.transaction(async (tx) => { ... })` entry point that wraps `BEGIN`, runs the user's callback with a `tx` handle exposing the same `query` / `insert` / `bulkInsert` surface as `db`, and either `COMMIT`s on resolve or `ROLLBACK`s on throw.

`tx` operations route to the same JSON-RPC session; the engine already detects in-tx state. No engine changes are required.

## Acceptance criteria

- [ ] `db.transaction(async (tx) => { await tx.insert('t', {...}) })` commits on success.
- [ ] `db.transaction(async (tx) => { await tx.insert('t', {...}); throw new Error('boom') })` rolls back; the row is not visible afterwards.
- [ ] An exception thrown by `tx.query()` itself triggers rollback (no double-commit, no swallowed error).
- [ ] The wrapper returns whatever the callback resolves to.
- [ ] Nested `db.transaction` calls produce a clear `NESTED_TX_NOT_SUPPORTED` error (snapshot isolation doesn't currently support nesting; document and fail loudly).
- [ ] Type surface (`index.d.ts`) is updated; `tx` has the same `query` / `insert` / `bulkInsert` types as `db`.
- [ ] Tests cover: success commit, throw rollback, query-failure rollback, return-value passthrough, nested-tx error.

## Blocked by

None - can start immediately.
