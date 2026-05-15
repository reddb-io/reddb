# Events, CDC, and transport `rid` vocabulary sweep [AFK]

GitHub issue: #498

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Make event payloads, CDC, MCP/gRPC/SDK result shapes, and public docs use the ADR 0019 vocabulary consistently after the row, document/KV, and graph tracers land.

## Acceptance criteria

- [x] Event payloads identify changed items with `rid`, `collection`, and `kind`.
- [x] CDC payloads identify changed items with `rid`, `collection`, and `kind`.
- [x] MCP/gRPC/SDK result shapes touched by item identity use `rid`.
- [x] Public docs touched by these surfaces use RedDB ID, `rid`, item, and item `kind`.
- [x] Regression tests cover at least one row event and one non-row item event/payload path.

## Blocked by

- #493
- #496
- #497
