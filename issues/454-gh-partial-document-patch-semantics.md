# Add partial document patch semantics [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/454

Labels: enhancement, ready-for-agent

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#449

## What to build

Add predictable partial patch semantics for documents. Users should be able to update nested document fields without replacing the full document body, through HTTP and the chosen SQL/runtime surface.

## Acceptance criteria

- [ ] Patch supports `set` of nested document paths.
- [ ] Patch supports `unset` of nested document paths.
- [ ] `set` creates intermediate objects when needed.
- [ ] `unset` on an absent field is a no-op.
- [ ] Full document replacement remains available separately.
- [ ] Patch returns the updated document envelope.
- [ ] Array positional update is explicitly unsupported with a helpful error or left undocumented.
- [ ] Runtime/HTTP tests cover nested set, nested unset, absent unset, persistence after patch, and error behavior.

## Blocked by

- #452

