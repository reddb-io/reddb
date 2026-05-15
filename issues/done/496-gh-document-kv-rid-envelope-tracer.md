# Document and KV `rid` envelope tracer [AFK]

GitHub issue: #496

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Extend the public item envelope and `rid` vocabulary to document and KV item paths. Document query/read/returning surfaces should expose `kind = document`; KV query/read/returning surfaces should expose `kind = kv`.

## Acceptance criteria

- [x] Document reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [x] KV reads expose `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at`.
- [x] Document item `kind` is `document`.
- [x] KV item `kind` is `kv`.
- [x] Public JSON/API/SDK-visible paths touched by document and KV reads use `rid`.
- [x] Tests cover document and KV query results through the public query path.

## Blocked by

- #493
- #495
