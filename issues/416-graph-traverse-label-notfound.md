# GRAPH TRAVERSE/SHORTEST_PATH 'label' not_found despite label exists [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/416

Labels: needs-triage

GitHub issue number: #416

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Bug

## Symptom

`GRAPH TRAVERSE '<label>'` returns "not_found" for labels that demonstrably exist in the same collection. The same label resolves correctly when its numeric id is supplied:

```sql
INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')
GRAPH TRAVERSE 'cinderella'              -- not_found
GRAPH NEIGHBORHOOD '177'                 -- resolves cinderella (id 177)
```

Same divergence for `GRAPH SHORTEST_PATH 'label' TO 'label'` — only numeric-id form works.

## Impact

High. Forces callers to maintain a label→id map in application code, defeating the ergonomics of label-based graph operations and contradicting the documented behavior.

## Acceptance criteria

- [ ] `GRAPH TRAVERSE '<label>'` resolves when a node with that label exists in the queried collection.
- [ ] `GRAPH SHORTEST_PATH '<label_a>' TO '<label_b>'` resolves identically to the numeric-id form.
- [ ] The same label lookup is used by `GRAPH NEIGHBORHOOD` and `GRAPH TRAVERSE` (single source of truth).
- [ ] Regression tests covering label resolution across all `GRAPH` algorithm commands.
