# Graph `rid`, `from_rid`, and `to_rid` tracer [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Move graph public vocabulary to ADR 0019. Graph nodes and edges should expose the public item envelope. Graph edge endpoints should be public as `from_rid` and `to_rid`, not `from` and `to`, across insert/read/query/returning surfaces touched by this tracer.

## Acceptance criteria

- [ ] Graph node reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` with `kind = node`.
- [ ] Graph edge reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` with `kind = edge`.
- [ ] Public graph edge insert/read examples and tested query paths use `from_rid` and `to_rid`.
- [ ] Graph identity/topology fields (`rid`, `label`, `from_rid`, `to_rid`) are treated as immutable in visible graph mutation paths available today.
- [ ] Tests cover node and edge result shapes and edge endpoint naming.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 467-reserved-system-fields-conflict-validation.md
