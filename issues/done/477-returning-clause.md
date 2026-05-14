# `RETURNING *` / `RETURNING <col>, ...` on INSERT / UPDATE / DELETE [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

Postgres-style `RETURNING` clause for mutations. Callers today need a second `SELECT` to learn the assigned id, computed defaults, or the new row state. Add a parser branch on `INSERT`, `UPDATE`, and `DELETE` that accepts:

- `RETURNING *` — return all columns of the affected row(s).
- `RETURNING <col>, <col>, …` — return the named columns.

The mutation executor already computes the affected row set; surface it through the same result codec used by `SELECT`. Arbitrary `RETURNING <expr>` (function calls, arithmetic) is out of scope for this slice per the parent PRD.

## Acceptance criteria

- [ ] `INSERT INTO t (col1, col2) VALUES (1, 'a') RETURNING id` returns a one-row result with the assigned id.
- [ ] `INSERT … RETURNING *` returns all columns of the new row.
- [ ] `UPDATE t SET col = 'x' WHERE id = 1 RETURNING id, col` returns the affected rows projected to the named columns.
- [ ] `DELETE FROM t WHERE id IN (1, 2) RETURNING *` returns the deleted rows.
- [ ] Multi-row mutations return multi-row `RETURNING` results.
- [ ] The result-row count matches the `affected` count in the same envelope.
- [ ] `RETURNING` on a row with computed defaults surfaces those defaults.
- [ ] `RETURNING <expr>` with arbitrary expressions produces a clear `NOT_YET_SUPPORTED` error pointing to a follow-up issue.
- [ ] Integration tests for each of INSERT / UPDATE / DELETE × RETURNING * / named-columns.

## Blocked by

None - can start immediately.
