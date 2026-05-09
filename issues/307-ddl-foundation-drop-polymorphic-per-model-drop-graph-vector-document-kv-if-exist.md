# DDL: foundation - DROP polymorphic + per-model DROP (graph/vector/document/kv) + IF EXISTS [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/307

Labels: needs-triage

GitHub issue number: #307

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#306

## What to build

Foundation slice - fecha cobertura de DROP em todos os 7 models + adiciona polymorphic `DROP COLLECTION`. Sem isto, slices subsequentes nao funcionam.

End-to-end:
- Parser: adiciona `DROP GRAPH`, `DROP VECTOR`, `DROP DOCUMENT`, `DROP KV` + `DROP COLLECTION` (polymorphic).
