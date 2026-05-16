# Events, CDC, and transport `rid` vocabulary sweep [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Make event payloads, CDC, MCP/gRPC/SDK result shapes, and public docs use the ADR 0019 vocabulary consistently after the row, document/KV, and graph tracers land.

## Acceptance criteria

- [ ] Event payloads identify changed items with `rid`, `collection`, and `kind`.
- [ ] CDC payloads identify changed items with `rid`, `collection`, and `kind`.
- [ ] MCP/gRPC/SDK result shapes touched by item identity use `rid`.
- [ ] Public docs touched by these surfaces use RedDB ID, `rid`, item, and item `kind`.
- [ ] Regression tests cover at least one row event and one non-row item event/payload path.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 468-document-kv-rid-envelope-tracer.md
- 469-graph-rid-from-rid-to-rid-tracer.md
