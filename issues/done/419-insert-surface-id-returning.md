# Surface inserted entity id on INSERT (or RETURNING * for graph) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/419

Labels: enhancement

GitHub issue number: #419

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

Surface the inserted entity id on every INSERT path so callers don't have to assume sequential ids.

Options (any/all):
- `db.insert(...)` returns `{ affected, id }` instead of `{ affected }`.
- `INSERT INTO ... NODE (...) VALUES (...) RETURNING *` works for graph inserts (currently errors "RETURNING is not yet supported for this INSERT path").
- Multi-row insert returns an `ids: [...]` array.

## Acceptance criteria

- [x] Single-row INSERT (table, document, graph node, graph edge, KV, vector) returns the assigned id.
- [x] `RETURNING *` parses and executes for graph insert paths.
- [x] Drivers expose the id in their idiomatic shape (`{affected, id}` in JS/Python/Rust).
- [x] Tests covering id surfacing across all entity kinds.
