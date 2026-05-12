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

- [ ] `StrictValidator` deep module: unit tests for every branch (ok, retry needed, retry exhausted, lenient warn-only).
- [ ] One retry budget enforced; never two retries.
- [ ] HTTP 422 returned on retry exhaustion with clear `validation.errors`.
- [ ] `ASK '...' STRICT OFF` works and surfaces warnings instead of erroring.
- [ ] Integration test with fake LLM that emits invalid `[^N]` on first call, valid on retry.
- [ ] Integration test where retry also fails → 422.

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

Slice 2 (this commit): parser/AST tracer for `ASK '...' STRICT ON|OFF`.

- Added `AskQuery.strict: bool`, defaulting to `true` for existing SQL and
  transport callers.
- Parsed `STRICT OFF` as the lenient citation-validation opt-in and
  `STRICT ON` as explicit strict mode, in any order with the existing ASK
  clauses.
- Added parser coverage for default strict mode, `STRICT OFF`, `STRICT ON`,
  and malformed `STRICT` clauses.
- Updated the gRPC ASK JSON-payload adapter to preserve existing strict
  behavior until the typed gRPC ASK schema slice lands.
- Fixed a pre-existing `pg_wire_ask_row_encoder` unit-test lifetime issue
  found during the red check so lib unit-test targets compile again.

Deferred to follow-up slices:

- Wire `ask.strict` into `execute_ask` and map it to
  `strict_validator::Mode::{Strict, Lenient}`.
- Issue the one retry LLM call on `Decision::Retry` and enforce the typed
  one-retry budget.
- Map `Decision::GiveUp` to HTTP 422 with `validation.errors` in the
  response body.
- Integration tests with a fake LLM provider emitting invalid markers first,
  then valid markers, still depend on the stubbable LLM transport refactor.

Verification (this slice):
- Red check first failed on missing `AskQuery.strict`.
- `cargo test -p reddb-io-server --lib test_parse_dml_extended_literals_auto_embed_and_ask_forms -- --nocapture`
  → 1 passed.
- `cargo test -p reddb-io-server --test ask_parser -- --nocapture`
  → 26 passed.
- `cargo test -p reddb-io-server --lib runtime::ai::pg_wire_ask_row_encoder -- --nocapture`
  → 25 passed.
- `cargo check -p reddb-io-server` clean.
- `pnpm test` ran and skipped because `target/debug/red` is not built.
- `pnpm typecheck` exits 1; `pnpm run typecheck` confirms the repo has no
  `typecheck` script. The direct TypeScript output said "No errors found".
