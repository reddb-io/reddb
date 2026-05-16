# Document and KV `rid` envelope tracer [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Extend the public item envelope and `rid` vocabulary to document and KV item paths. Document query/read/returning surfaces should expose `kind = document`; KV query/read/returning surfaces should expose `kind = kv`.

## Acceptance criteria

- [ ] Document reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [ ] KV reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [ ] Document item `kind` is `document`.
- [ ] KV item `kind` is `kv`.
- [ ] Public JSON/API/SDK-visible paths touched by document and KV reads use `rid`.
- [ ] Tests cover document and KV query results through the public query path.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 467-reserved-system-fields-conflict-validation.md
