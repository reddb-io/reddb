# Document and KV compound updates [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/502

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Make explicit document and KV update targets work end to end with compound assignment, `WHERE`, `RETURNING`, atomicity, and the available permission/RLS hooks.

## Acceptance criteria

- [x] `UPDATE <collection> DOCUMENTS SET <field> += ... WHERE ... RETURNING ...` works for top-level document fields.
- [x] `UPDATE <collection> KV SET value += ... WHERE key = ... RETURNING ...` works for numeric KV values.
- [x] Document and KV updates use post-image `RETURNING`.
- [x] Missing, null, non-numeric, division-by-zero, modulo-by-zero, and overflow failures abort the whole statement.
- [x] Document and KV update `WHERE` see the documented top-level item shapes.
- [x] Available authorization/RLS checks use the explicit target.
- [x] Tests cover document and KV positive updates, invalid inputs, and atomic failure.

## Blocked by

- #496
- #494
- #499
- #501
