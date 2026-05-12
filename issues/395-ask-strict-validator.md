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
- Thread `AskQuery.strict` into `execute_ask` and map it to
  `strict_validator::Mode`.
- Integration tests with a fake LLM provider (depends on transport
  refactor).

The deep module is the load-bearing piece; the remaining slices are
mechanical wiring and can land independently. Issue stays open with
this progress note.

Slice 2 (this commit): SQL/gRPC request surface for strict mode.

- Added `strict: Option<bool>` to `AskQuery`; `None` preserves the
  default strict policy, `Some(false)` represents `STRICT OFF`, and
  `Some(true)` represents explicit `STRICT ON`.
- Parser now accepts `ASK '...' STRICT OFF` and `STRICT ON` in the
  same optional-clause loop as `USING`, `LIMIT`, `TEMPERATURE`, and
  `SEED`.
- Parser tests pin `STRICT OFF`, order-independent `STRICT ON`, default
  `None`, and syntax errors for missing/unknown strict mode values.
- gRPC's legacy JSON ASK payload binder forwards optional `"strict":
  bool` into `AskQuery` so non-SQL callers can request the same mode.
- Fixed two PG-wire ASK encoder unit-test temporary borrows that blocked
  the server test harness from compiling.
- Changed the workspace-internal gRPC connector's `QueryRequest`
  construction to start from `Default`; this avoids stale generated
  proto artifacts disagreeing on optional trailing fields during local
  test/check runs.

Deferred to follow-up slices:

- Thread `AskQuery.strict` into `execute_ask` and map it to
  `strict_validator::Mode`.
- Wire `validate()` into `execute_ask` and issue the single retry LLM
  call.
- Map retry exhaustion to HTTP 422 with `validation.errors`.
- Integration tests with a fake LLM provider still depend on the
  stubbable transport refactor.

Verification:

- `cargo test -p reddb-io-server --lib storage::query::parser::tests::test_parse_dml -- --nocapture`
  → 2 passed, 3982 filtered out.
- `cargo test -p reddb-io-server --lib runtime::ai::pg_wire_ask_row_encoder -- --nocapture`
  → 25 passed, 3959 filtered out.
- `cargo check -p reddb-io-server` → clean.
- `pnpm test` → command ran; JS smoke test skipped because
  `target/debug/red` is not built.
- `pnpm typecheck` → unavailable; root `package.json` has no
  `typecheck` script.
