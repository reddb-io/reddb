# Document and KV compound updates [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Make explicit document and KV update targets work end to end with compound assignment, `WHERE`, `RETURNING`, atomicity, and the available permission/RLS hooks.

## Acceptance criteria

- [ ] `UPDATE <collection> DOCUMENTS SET <field> += ... WHERE ... RETURNING ...` works for top-level document fields.
- [ ] `UPDATE <collection> KV SET value += ... WHERE key = ... RETURNING ...` works for numeric KV values.
- [ ] Document and KV updates use post-image `RETURNING`.
- [ ] Missing, null, non-numeric, division-by-zero, modulo-by-zero, and overflow failures abort the whole statement.
- [ ] Document and KV update `WHERE` see the documented top-level item shapes.
- [ ] Available authorization/RLS checks use the explicit target.
- [ ] Tests cover document and KV positive updates, invalid inputs, and atomic failure.

## Blocked by

- 468-document-kv-rid-envelope-tracer.md
- 471-postgres-compatible-math-functions.md
- 472-compound-assignment-row-updates.md
- 474-explicit-update-target-parser-validation.md
