# docs↔parser drift: GRAPH TRAVERSE FROM '...' STRATEGY bfs parse-errors [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/417

Labels: enhancement

GitHub issue number: #417

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Bug — docs↔parser drift

## Symptom

The documented `GRAPH TRAVERSE FROM '<id>' STRATEGY bfs ...` syntax parse-errors:

```
GRAPH TRAVERSE FROM 'alice' STRATEGY bfs LIMIT 10
-- Parse error: Unexpected token: FROM (expected: string)
```

Either the parser must accept the `FROM ... STRATEGY` form documented in the guide, or the docs must be corrected to show only the supported `GRAPH TRAVERSE '<id>' ...` form.

## Impact

High. Users following docs hit parse errors on their first traversal query.

## Acceptance criteria

- [ ] Resolve the drift: either implement the documented `FROM ... STRATEGY` form, or update docs to match the parser.
- [ ] Tests pinning the supported `GRAPH TRAVERSE` grammar so future drift is caught in CI.
