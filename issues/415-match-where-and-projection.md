# MATCH WHERE ignored and RETURN n.foo projects empty {} [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/415

Labels: needs-triage

GitHub issue number: #415

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Bug

## Symptom

`MATCH (n) WHERE n.label = 'X' RETURN n` returns every row in the collection, ignoring the `WHERE` predicate. Additionally, each returned row is `{}` — projecting `n.name`, `n.label`, etc. yields empty objects.

```sql
MATCH (n) WHERE n.label = 'arc_predator' RETURN n
-- returns every IS_ARCHETYPE row, all as {}
```

## Impact

Critical. `MATCH` is currently documentation-only — it cannot filter and cannot project. Users have no Cypher-style way to read graph data.

## Acceptance criteria

- [ ] `MATCH (n) WHERE n.label = 'X' RETURN n` returns ONLY rows matching `n.label = 'X'`.
- [ ] `MATCH (n) RETURN n.name` returns `[{ "name": "..." }, ...]` with actual property values.
- [ ] `RETURN n` (whole entity) returns the full property bag, not `{}`.
- [ ] Regression tests for both `WHERE` filtering and per-property projection.
