# Read API: SELECT row-projection returns empty for graph collections [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/414

Labels: enhancement

GitHub issue number: #414

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Bug

## Symptom

`SELECT <col> FROM <coll> WHERE <pred>` returns `[]` against a collection holding graph entities (NODE/EDGE inserts), even when those rows demonstrably exist (confirmed via `GRAPH NEIGHBORHOOD '<id>'` resolving them).

```sql
INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')
SELECT label, name FROM tales WHERE label = 'cinderella'  -- returns []
SELECT * FROM tales                                       -- returns []
SELECT node_type, COUNT(*) FROM tales GROUP BY node_type  -- works
```

Aggregates work; row-projection scans return nothing.

## Impact

Critical. Without row-projection on graph collections, callers cannot read inserted data back without (a) round-tripping through a `GRAPH ...` algorithm command, or (b) maintaining an in-memory id map at ingest time. Both are workarounds for a primitive read.

## Acceptance criteria

- [ ] `SELECT * FROM tales` returns one row per graph entity (NODE + EDGE) with the standard fields including projected properties.
- [ ] `SELECT label, name FROM tales WHERE label = 'X'` returns rows whose `label = 'X'`.
- [ ] Existing aggregate behavior remains correct.
- [ ] Regression test covering both row-projection and aggregate paths on a graph-only collection.
