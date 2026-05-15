# Ordered multi-model update batches [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Extend ordered update batch semantics from rows to documents, KV, graph nodes, and graph edges. Each target should support `ORDER BY` with `LIMIT`, top-level order fields only, and implicit `rid ASC` tie-breaking.

## Acceptance criteria

- [ ] `DOCUMENTS`, `KV`, `NODES`, and `EDGES` updates accept `ORDER BY ... LIMIT`.
- [ ] `ORDER BY` without `LIMIT` is rejected for each target.
- [ ] Expression and nested-path ordering are rejected in this slice.
- [ ] Ties are broken by implicit `rid ASC` when `rid` is absent.
- [ ] Tests cover at least one ordered batch per non-row target.

## Blocked by

- #500
- #502
- #503
