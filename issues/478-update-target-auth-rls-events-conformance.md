# Authorization, RLS, masking, and event conformance pack [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Add focused conformance coverage for the completed multi-model update surface. Explicit update targets should drive authorization, RLS, masking, events, CDC, indexes, WAL, and recovery as materialized updates.

## Acceptance criteria

- [ ] Authorization tests prove explicit targets are used for row, document, KV, node, and edge updates where supported.
- [ ] RLS/masking tests prove `RETURNING` does not bypass policy.
- [ ] Event/CDC tests prove multi-model updates emit ordinary materialized update payloads with `rid`, `collection`, and `kind`.
- [ ] Index recheck tests cover changed indexed fields for at least row plus one non-row target.
- [ ] WAL/recovery or persistence tests prove replay observes materialized post-images, not symbolic compound operations.

## Blocked by

- 475-document-kv-compound-updates.md
- 476-graph-node-edge-compound-updates.md
- 477-ordered-multimodel-update-batches.md
