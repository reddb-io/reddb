# `CURRENT_TIMESTAMP` / `CURRENT_DATE` / `CURRENT_TIME` as scalar functions [AFK]

Labels: bug, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

Today `SELECT CURRENT_TIMESTAMP` is parsed as a bare column reference. The executor cannot resolve it and falls back to a multi-table system-collection scan that dumps every row of `red_stats`, `red_config`, and the user's tables. The user expected a single-row scalar like `SELECT NOW()` returns.

Recognize `CURRENT_TIMESTAMP`, `CURRENT_DATE`, and `CURRENT_TIME` as zero-argument function calls in the expression parser and route them to the same time source used by `NOW()`. Semantics:

- `CURRENT_TIMESTAMP` returns the same value as `NOW()` (epoch ms).
- `CURRENT_DATE` extracts the date portion (string in `YYYY-MM-DD` form).
- `CURRENT_TIME` extracts the time portion (string in `HH:MM:SS.mmm` form or epoch-ms-since-midnight; pick one and document).

## Acceptance criteria

- [ ] `SELECT CURRENT_TIMESTAMP` returns a single row with one column holding an integer close to `NOW()`.
- [ ] `SELECT CURRENT_DATE` returns a single row with a date-shaped string.
- [ ] `SELECT CURRENT_TIME` returns a single row with a time-shaped value (semantics documented).
- [ ] None of the three forms dumps system tables.
- [ ] All three forms work inside expressions: `SELECT * FROM t WHERE created_at >= CURRENT_TIMESTAMP - 86400000`.
- [ ] Cross-check test: `SELECT CURRENT_TIMESTAMP` and `SELECT NOW()` return values within a few milliseconds of each other.

## Blocked by

None - can start immediately.
