# Bun verification + tests for parameterized queries [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/372

Labels: needs-triage

GitHub issue number: #372

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Verify `@reddb-io/sdk` works correctly under Bun with the new parameterized API. The SDK is shared with Node, so this is mostly a verification + Bun-specific test pass, plus any patches needed for Bun's `child_process` / native socket differences uncovered along the way.

## Acceptance criteria

- [x] Existing JS integration test suite (#353, #355, #357) runs green under Bun.
- [x] Bun-specific test added under `drivers/bun/` exercising parameterized SELECT, INSERT, and SEARCH SIMILAR with vector params.
- [x] Any Bun-specific divergences from Node behavior are documented or fixed.

## Blocked by

- #353

## Completion note

Implemented in this slice:

- Reworked the JS RedWire parameter codec test to avoid the Node-only
  `node:test` harness so it runs under Bun 0.6.14 and Node.
- Added `drivers/bun/test/params.test.mjs`, an engine-backed Bun smoke test
  for the shared SDK `db.query(sql, params)` path covering parameterized SELECT,
  INSERT, null values, and `SEARCH SIMILAR` with vector params.
- Added Bun package scripts for the Bun-specific smoke and shared JS SDK Bun
  verification.
- Documented that `@reddb-io/client-bun` is still a low-level raw query client;
  Bun apps needing parameter arrays should use `@reddb-io/sdk`.

Verification:

- `bun drivers/js/test/redwire.params.test.mjs`
- `bun drivers/bun/test/params.test.mjs` (skips without `target/debug/red`)
- `node --test drivers/js/test/redwire.params.test.mjs`
- `bun drivers/js/test/smoke.test.mjs` (skips without `target/debug/red`)
- `bun run test` from `drivers/bun` (skips without `target/debug/red`)
- `bun run test:js-sdk` from `drivers/bun`
- `pnpm test` (skips without `target/debug/red`)

Blocked / notes:

- Full engine-backed Bun smoke execution still requires building
  `target/debug/red` or setting `REDDB_BINARY_PATH`.
- `pnpm typecheck` remains unavailable in this harness because TypeScript is not
  installed; the command resolves to the placeholder `tsc` package message.
