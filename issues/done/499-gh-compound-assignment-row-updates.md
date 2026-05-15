# Compound assignment for row updates [AFK]

GitHub issue: #499

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#492

## What to build

Support `+=`, `-=`, `*=`, `/=`, and `%=` for ordinary row `UPDATE`. Compound assignment should be top-level only, evaluate against the row pre-image, require an existing non-null numeric left-hand field, and persist as an ordinary materialized update.

## Acceptance criteria

- [x] Row `UPDATE` accepts `SET x += expr`, `-=`, `*=`, `/=`, and `%=`.
- [x] Compound assignment produces the same post-image as the equivalent explicit expression assignment.
- [x] Multiple assignments read the pre-image, not earlier assignments in the same statement.
- [x] Missing, null, and non-numeric left-hand fields fail the statement.
- [x] Division by zero, modulo by zero, and overflow fail the statement.
- [x] Indexes/events/WAL observe a normal materialized row update.
- [x] Tests cover positive operations, invalid inputs, pre-image semantics, and atomic failure.

## Blocked by

- #493
- #494
