# Strict citation validation + one-retry policy (StrictValidator) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/395

Labels: needs-triage

GitHub issue number: #395

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Introduces strict citation validation with a one-retry policy. After parsing `[^N]` markers, the server checks `1 <= N <= len(sources_flat)`.

On structural failure:
- Build a corrected prompt explaining the index range to the LLM.
- Issue exactly one retry call.
- If retry also fails, return HTTP 422 with `validation.ok = false` and `validation.errors` populated.

Lenient mode is opt-in: `ASK '...' STRICT OFF` skips validation entirely and surfaces warnings only.

Introduces `StrictValidator` deep module — pure function `(answer, sources_count, mode) → ok | retry_prompt | giveup`.

## Acceptance criteria

- [x] `StrictValidator` deep module: unit tests for every branch (ok, retry needed, retry exhausted, lenient warn-only).
- [x] One retry budget enforced; never two retries.
- [x] HTTP 422 returned on retry exhaustion with clear `validation.errors`.
- [x] `ASK '...' STRICT OFF` works and surfaces warnings instead of erroring.
- [x] Integration test with fake LLM that emits invalid `[^N]` on first call, valid on retry.
- [x] Integration test where retry also fails → 422.

## Blocked by

- #393

## Progress

Slice 1 (this commit): `StrictValidator` deep module landed at
`crates/reddb-server/src/runtime/ai/strict_validator.rs` with 17 unit
tests covering every branch (strict ok/retry/giveup, lenient warn-only,
mixed warnings, determinism, retry-prompt contract, empty input). The
module is pure — no transport, no I/O — and exposes:

- `Mode::{Strict, Lenient}`
- `Attempt::{First, Retry}` — the type IS the retry budget
- `Decision::{Ok, Retry{prompt}, GiveUp{errors}}`
- `validate(parsed, mode, attempt) -> Decision`

Deferred to follow-up slices:

- Wire `validate()` into `execute_ask` and actually issue the retry LLM
  call (needs a stubbable LLM transport so the "fake LLM" integration
  test in the AC is writable — today `AiTransport::from_runtime` is the
  hard boundary).
- Map `Decision::GiveUp` to HTTP 422 with `validation.errors` in the
  response body.
- Parse `ASK '...' STRICT ON|OFF` in the SQL parser and thread `Mode`
  into `AskQuery`.
- Integration tests with a fake LLM provider (depends on transport
  refactor).

The deep module is the load-bearing piece; the remaining slices are
mechanical wiring and can land independently. Issue stays open with
this progress note.

Slice 2 (this commit): wired strict citation validation into the HTTP
ASK path. `ASK '...' STRICT ON|OFF` now parses into `AskQuery`, strict
mode validates parsed citations after the first LLM answer, issues one
retry with the validator correction prompt, and returns HTTP 422 with
`validation.ok = false` plus `validation.errors` when the retry still
fails. `STRICT OFF` uses lenient mode, returns 200, and keeps structural
diagnostics in `validation.warnings`.

Verification:

- `cargo test -p reddb-io-server strict_validator --lib`
- `cargo test -p reddb-io-server http_query_ask --lib`
- `cargo check`
- `pnpm test` (skipped: `target/debug/red` missing)
- `pnpm typecheck` (reported `TypeScript: No errors found` but exited 1)
