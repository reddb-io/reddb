# Postgres-compatible math functions [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/494
Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Add the ADR 0019 numeric scalar function package through the normal expression path. Canonical Postgres-compatible names should work, ergonomic aliases should resolve, and invalid numeric results should fail clearly.

## Acceptance criteria

- [ ] `SQRT`, `POWER`, `EXP`, `LN`, `LOG`, `LOG10`, trigonometric functions, angle conversion functions, and `PI()` execute through normal SQL expressions.
- [ ] `POW`, `ARCSIN`, `ARCCOS`, and `ARCTAN` aliases work.
- [ ] Advanced math functions return `Float`.
- [ ] Division/modulo by zero, invalid domains, overflow, `NaN`, and infinity surface as errors.
- [ ] Tests cover canonical names, aliases, return type behavior, and representative error cases.

## Blocked by

None - can start immediately.
