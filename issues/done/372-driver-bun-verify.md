# Bun verification + tests for parameterized queries [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/372

Labels:

GitHub issue number: #372

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Verify `@reddb-io/sdk` works correctly under Bun with the new parameterized API. The SDK is shared with Node, so this is mostly a verification + Bun-specific test pass, plus any patches needed for Bun's `child_process` / native socket differences uncovered along the way.

## Acceptance criteria

- [ ] Existing JS integration test suite (#353, #355, #357) runs green under Bun.
- [ ] Bun-specific test added under `drivers/bun/` exercising parameterized SELECT, INSERT, and SEARCH SIMILAR with vector params.
- [ ] Any Bun-specific divergences from Node behavior are documented or fixed.

## Blocked by

- #353
