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

- [ ] Existing JS integration test suite (#353, #355, #357) runs green under Bun.
- [ ] Bun-specific test added under `drivers/bun/` exercising parameterized SELECT, INSERT, and SEARCH SIMILAR with vector params.
- [ ] Any Bun-specific divergences from Node behavior are documented or fixed.

## Blocked by

- #353

## Progress

Completed in this slice.

- Added `drivers/bun/params.test.ts`, a Bun-native embedded SDK smoke that
  exercises parameterized `SELECT`, SQL `INSERT`, and `SEARCH SIMILAR` with
  vector params.
- Added Bun package scripts for the new params smoke, the shared JS RedWire
  params codec smoke, and the shared JS embedded smoke.
- Fixed the shared embedded bound-query path so parameterized `INSERT` and
  `SEARCH SIMILAR` dispatch through their normal executors instead of the
  select-only prepared-statement dispatcher.
- Made `drivers/js/test/redwire.params.test.mjs` runnable as a plain Bun
  script while preserving Node's `node:test` path, because Bun 0.6.x does not
  provide a usable `node:test` shim for this file.
- Updated the JS vector smoke assertions to match the current
  `SEARCH SIMILAR` result shape (`entity_id`, `score`) rather than expecting a
  stored `content` column that the command does not return.

Verification:

- `cargo build --bin red`
- `cargo check`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red bun drivers/bun/params.test.ts`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red bun drivers/js/test/smoke.test.mjs`
- `bun drivers/js/test/redwire.params.test.mjs`
- `bun drivers/js/test/redwire.smoke.mjs` (skipped as designed; requires `REDWIRE_E2E=1` and a running server)
- `node --test drivers/js/test/redwire.params.test.mjs`
- `git diff --check`
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red pnpm test`
- `pnpm typecheck` (nonzero: root command `typecheck` not found)

Note:

- `cargo test -p reddb-io-server query_with_params --lib` did not reach these
  new regressions because the existing server lib test build currently fails
  in `runtime/ai/pg_wire_ask_row_encoder.rs` with E0716 temporary borrow
  errors unrelated to this slice.
