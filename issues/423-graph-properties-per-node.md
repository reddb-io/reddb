# GRAPH PROPERTIES '<id-or-label>' per-node property lookup [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/423

Labels: needs-triage

GitHub issue number: #423

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

`GRAPH PROPERTIES '<id-or-label>'` returns the full property bag of a specific node:

```sql
GRAPH PROPERTIES '177'
GRAPH PROPERTIES 'cinderella'
```

Today: parse error. Only the bare `GRAPH PROPERTIES` (no argument) works, returning graph-wide stats.

## Acceptance criteria

- [ ] `GRAPH PROPERTIES '<id>'` returns all properties of the node.
- [ ] `GRAPH PROPERTIES '<label>'` resolves via the same label index as `GRAPH NEIGHBORHOOD`.
- [ ] Clear error when id/label does not exist.
- [ ] Tests covering id form, label form, missing-id, ambiguous-label.
