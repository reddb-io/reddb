# Authorization, RLS, masking, and event conformance pack [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/505

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Add focused conformance coverage for the completed multi-model update surface. Explicit update targets should drive authorization, RLS, masking, events, CDC, indexes, WAL, and recovery as materialized updates.

## Acceptance criteria

- [x] Authorization tests prove explicit targets are used for row, document, KV, node, and edge updates where supported.
- [x] RLS/masking tests prove `RETURNING` does not bypass policy.
- [x] Event/CDC tests prove multi-model updates emit ordinary materialized update payloads with `rid`, `collection`, and `kind`.
- [x] Index recheck tests cover changed indexed fields for at least row plus one non-row target.
- [x] WAL/recovery or persistence tests prove replay observes materialized post-images, not symbolic compound operations.

## Blocked by

- #502
- #503
- #504
