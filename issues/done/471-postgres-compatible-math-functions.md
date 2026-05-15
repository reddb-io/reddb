# Postgres-compatible math functions [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

issues/prd/rid-and-multimodel-update-surface.md

## What to build

Add the ADR 0019 numeric scalar function package through the normal expression path. Canonical Postgres-compatible names should work, ergonomic aliases should resolve, and invalid numeric results should fail clearly.

## Acceptance criteria

- [x] `SQRT`, `POWER`, `EXP`, `LN`, `LOG`, `LOG10`, trigonometric functions, angle conversion functions, and `PI()` execute through normal SQL expressions.
- [x] `POW`, `ARCSIN`, `ARCCOS`, and `ARCTAN` aliases work.
- [x] Advanced math functions return `Float`.
- [x] Division/modulo by zero, invalid domains, overflow, `NaN`, and infinity surface as errors.
- [x] Tests cover canonical names, aliases, return type behavior, and representative error cases.

## Blocked by

None - can start immediately.

## Resolution

Duplicate of issue 494, landed in commit 23141120 ("feat(sql): add postgres-compatible math scalars"). All AC met by:

- Function catalog entries in `crates/reddb-server/src/storage/schema/function_catalog.rs` (SQRT, POWER/POW, EXP, LN, LOG, LOG10, SIN, COS, TAN, ASIN/ARCSIN, ACOS/ARCCOS, ATAN/ARCTAN, ATAN2, COT, DEGREES, RADIANS, PI).
- Dispatch + finite-result checks in `crates/reddb-server/src/storage/query/evaluator.rs` (unary_math, binary_math, checked_math_result).
- Coverage in `tests/e2e_postgres_math_functions.rs` — canonical, alias, and error-case suites all pass on 2026-05-15.

Verification: `cargo test --test e2e_postgres_math_functions` — 3 passed.
