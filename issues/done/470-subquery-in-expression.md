# Subquery in expression context (uncorrelated) [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

Subqueries work in `FROM` (`FROM (SELECT …) AS alias`) but are rejected in expression contexts. The user's example:

```
SELECT * FROM tt WHERE id IN (SELECT id FROM tt WHERE name = 'a')
→ Parse error at 1:31: Unexpected token: SELECT
```

Extend the expression parser so a parenthesised `SELECT` in expression context is accepted, and add executor support to run the inner query before the outer comparison. Scope is **uncorrelated** subqueries only (the inner SELECT does not reference outer columns); correlated subqueries are a follow-up out of scope per the parent PRD.

## Acceptance criteria

- [x] `SELECT * FROM t WHERE id IN (SELECT id FROM other WHERE name = 'x')` parses and returns the expected rows.
- [x] `SELECT * FROM t WHERE col = (SELECT MAX(value) FROM other)` parses and executes (scalar subquery).
- [x] `SELECT col, (SELECT COUNT(*) FROM other) AS n FROM t` parses (scalar in projection).
- [x] A scalar subquery that returns more than one row produces a clear error.
- [x] A correlated subquery (inner SELECT references outer column) produces a clear `NOT_YET_SUPPORTED` error pointing to a follow-up issue.
- [x] Tests cover: `IN (subquery)`, `= (subquery)`, scalar subquery in projection, multi-row scalar error, correlated rejection.

## Blocked by

None - can start immediately.
