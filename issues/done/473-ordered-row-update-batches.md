# Ordered row update batches [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Add deterministic `ORDER BY ... LIMIT` support for row updates. Ordered row update batches should accept top-level order fields, reject `ORDER BY` without `LIMIT`, and add `rid ASC` as the implicit tie-breaker when absent.

## Acceptance criteria

- [ ] Row `UPDATE ... ORDER BY <top-level-field> [ASC|DESC] LIMIT N` updates the expected batch.
- [ ] `ORDER BY` without `LIMIT` is rejected for row updates.
- [ ] Non-top-level or expression order terms are rejected in this slice.
- [ ] Ties are broken by implicit `rid ASC` when `rid` is absent.
- [ ] Tests cover ordering, limiting, rejection cases, and deterministic tie behavior.

## Blocked by

- 466-rid-row-envelope-tracer.md
- 472-compound-assignment-row-updates.md
