# Row `rid` envelope tracer [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Introduce the new RedDB ID vocabulary on the ordinary row path end to end. Query results, `SELECT *`, `RETURNING *`, HTTP query JSON, and row-facing docs should expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` for rows, with `kind = row`. The public row path should stop exposing older public identifier names for this tracer.

## Acceptance criteria

- [x] Row query results expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [x] `SELECT *` and `RETURNING *` on rows include the public item envelope.
- [x] Row `kind` is `row`.
- [x] Public row query JSON uses `rid` as the RedDB ID field.
- [x] Existing row update/select tests are updated to use `rid` where they target the public identifier.
- [x] Docs touched by the row tracer use RedDB ID / `rid` vocabulary.

## Blocked by

None - can start immediately.
