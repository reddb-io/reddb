# Explicit update target parser and validation [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Parse and validate explicit item-kind update targets: `ROWS`, `DOCUMENTS`, `KV`, `NODES`, and `EDGES`. Omitted target remains rows. Collection model compatibility should be checked before mutation.

## Acceptance criteria

- [ ] Parser accepts `UPDATE <collection> ROWS|DOCUMENTS|KV|NODES|EDGES SET ...`.
- [ ] Parser preserves omitted-target row update behavior.
- [ ] Runtime validation rejects incompatible target/model combinations before mutation.
- [ ] Graph collections accept both `NODES` and `EDGES`.
- [ ] Generic or mixed collections can accept explicit item-kind targets where supported.
- [ ] Cross-kind update forms such as `UPDATE FROM ANY` remain rejected.
- [ ] Parser and runtime tests cover positive and negative target cases.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 468-document-kv-rid-envelope-tracer.md
- 469-graph-rid-from-rid-to-rid-tracer.md
