# Explicit update target parser and validation [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/501

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Parse and validate explicit item-kind update targets: `ROWS`, `DOCUMENTS`, `KV`, `NODES`, and `EDGES`. Omitted target remains rows. Collection model compatibility should be checked before mutation.

## Acceptance criteria

- [x] Parser accepts `UPDATE <collection> ROWS|DOCUMENTS|KV|NODES|EDGES SET ...`.
- [x] Parser preserves omitted-target row update behavior.
- [x] Runtime validation rejects incompatible target/model combinations before mutation.
- [x] Graph collections accept both `NODES` and `EDGES`.
- [x] Generic or mixed collections can accept explicit item-kind targets where supported.
- [x] Cross-kind update forms such as `UPDATE FROM ANY` remain rejected.
- [x] Parser and runtime tests cover positive and negative target cases.

## Blocked by

- #493
- #496
- #497
