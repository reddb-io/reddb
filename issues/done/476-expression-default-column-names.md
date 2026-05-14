# Expression default column names use source text, not operator tag [AFK]

Labels: enhancement, breaking-change, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

This is a **breaking change** for callers that look up result columns by the current operator-tag default names (`MUL`, `CONCAT`, `UPPER`, …). Document it in the changelog; the AFK call is "just flip it" because no production caller can reasonably depend on the surprising defaults.

## Parent

#466

## What to build

Today the result-set column name for an unaliased projection is the operator / function tag:

```
SELECT UPPER(name) FROM t LIMIT 1   → { "UPPER": "A" }
SELECT id * 2 FROM t LIMIT 1        → { "MUL": 16 }
SELECT name || '!' FROM t LIMIT 1   → { "CONCAT": "n8!" }
SELECT COALESCE(name, 'fb')         → { "COALESCE": "n8" }
```

Change the renderer policy: if the projection has an explicit `AS <alias>`, use the alias; otherwise, render the projection's source-text form. So `SELECT UPPER(name)` produces `{ "UPPER(name)": "A" }`, `SELECT id * 2` produces `{ "id * 2": 16 }`, and so on.

## Acceptance criteria

- [ ] `SELECT UPPER(name) FROM t LIMIT 1` returns a column named `UPPER(name)`.
- [ ] `SELECT id * 2 FROM t LIMIT 1` returns a column named `id * 2`.
- [ ] `SELECT name || '!' FROM t LIMIT 1` returns a column named `name || '!'`.
- [ ] `SELECT COALESCE(name, 'fb') FROM t LIMIT 1` returns a column named `COALESCE(name, 'fb')`.
- [ ] `SELECT UPPER(name) AS upn FROM t LIMIT 1` returns a column named `upn` (alias still overrides).
- [ ] Existing tests that pin operator-tag names are updated to the new policy.
- [ ] Changelog entry documents the breaking change.

## Blocked by

None - can start immediately.
