# EDGE insert accepts labels in from/to (not only numeric ids) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/420

Labels: needs-triage

GitHub issue number: #420

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

Accept node labels (not just numeric ids) in `INSERT INTO <coll> EDGE (label, from, to) VALUES (...)`:

```sql
INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', 'alice', 'bob')
```

Today this errors: `column 'from' expected integer, got 'alice'`. Combined with #B4 (label lookup broken in TRAVERSE), users must hardcode id offsets or round-trip through algorithm queries.

## Acceptance criteria

- [ ] `EDGE` insert accepts labels in `from`/`to`; engine resolves to ids using the same label index that `GRAPH NEIGHBORHOOD` uses.
- [ ] Numeric-id form remains supported.
- [ ] Clear error when label is ambiguous (multiple nodes with same label) or absent.
- [ ] Tests covering label, numeric, mixed, ambiguous, and missing cases.
