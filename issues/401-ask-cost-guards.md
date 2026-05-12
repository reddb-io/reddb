# Cost guards: settings + 413/504 errors (CostGuardEvaluator) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/401

Labels: enhancement

GitHub issue number: #401

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Hard limits on ASK resource consumption, configurable per deployment.

Settings introduced:
- `ask.max_prompt_tokens` (default 8192)
- `ask.max_completion_tokens` (default 1024)
- `ask.max_sources_bytes` (default 262144)
- `ask.timeout_ms` (default 30000)
- `ask.daily_cost_cap_usd` (default unlimited, per tenant)

Exceeded limits return HTTP 413 (over-budget) or 504 (timeout) with the offending limit named. Daily cost counter resets at UTC midnight (deterministic clock injection in tests).

Introduces `CostGuardEvaluator` deep module — pure `(usage_so_far, daily_state, settings, now) → allow | reject`.

## Acceptance criteria

- [ ] `CostGuardEvaluator` deep module: unit tests for every threshold, multi-tenant isolation, UTC midnight reset with injected clock.
- [ ] Each limit produces a distinct, named error (413 with which limit; 504 with timeout name).
- [ ] Daily cap is per tenant.
- [ ] Integration test: a query exceeding `max_prompt_tokens` returns 413 with field-named error.
- [ ] Integration test: a provider call exceeding `timeout_ms` returns 504.

## Blocked by

- #393

## Progress

Slice 1: `CostGuardEvaluator` deep module landed at
`crates/reddb-server/src/runtime/ai/cost_guard.rs` with 18 unit tests
covering every branch. Pure — no I/O, no clock reads. Exposes:

- `Settings { max_prompt_tokens, max_completion_tokens, max_sources_bytes,
  timeout_ms, daily_cost_cap_usd }` with the spec defaults
  (8192 / 1024 / 262144 / 30000 / None).
- `Usage`, `DailyState`, `Now` plain-data inputs (injected clock).
- `LimitKind` with `field_name()` → operator-visible config key and
  `http_status()` → 413 for over-budget caps, 504 for timeout.
- `Decision::{Allow, Reject { limit, http_status, detail }}`.
- `evaluate(usage, daily, settings, now) -> Decision`.

Check order is fixed and tested: prompt → sources → completion →
timeout → daily cap. First breach wins.

Tests cover: at-limit allowed, one-over for each cap, daily-cap
boundary (strict `>`), UTC-midnight reset via `div_euclid` on
`SECS_PER_DAY`, multi-tenant isolation (separate `DailyState`s never
interact because the evaluator holds no state), check-order pins,
field/HTTP mapping, deterministic re-runs, and negative epoch
correctness for `same_utc_day`.

Deferred to follow-up slices:

- Wire `evaluate()` into `execute_ask` at the three checkpoints
  (pre-call after prompt assembly, in-flight on each streamed chunk,
  post-call when accruing daily spend).
- Per-tenant `DailyState` registry + reset bookkeeping.
- Map `Decision::Reject` to HTTP 413/504 with `limit.field_name()`
  in the error body.
- Settings plumbing — surface `ask.max_*` / `ask.timeout_ms` /
  `ask.daily_cost_cap_usd` in runtime config.
- Integration tests with prompt over `max_prompt_tokens` (413) and
  provider call exceeding `timeout_ms` (504).

Deep module is the load-bearing piece; remaining slices are
mechanical wiring and can land independently. Issue stays open with
this progress note (mirrors slice 1 pattern of #395).

Slice 2: pre-call ASK prompt/source guard wiring landed.

- Surfaced `ask.max_prompt_tokens`, `ask.max_completion_tokens`,
  `ask.max_sources_bytes`, `ask.timeout_ms`, and
  `ask.daily_cost_cap_usd` in the runtime config matrix as optional
  deployment knobs.
- Added a runtime `config_f64` reader for the daily USD setting.
- Wired `CostGuardEvaluator::evaluate()` into `execute_ask` immediately
  after prompt/source assembly and before provider credential lookup.
  This slice enforces prompt-token and source-byte caps at the pre-call
  checkpoint.
- Mapped cost guard rejections to `RedDBError::QuotaExceeded` with the
  limit field name in the payload. `/query` now uses the shared
  runtime error mapper so `payload` quota breaches return HTTP 413.
- Added HTTP handler coverage:
  `ask.max_prompt_tokens = 1` plus `ASK 'why did login fail?'` returns
  413 and includes `max_prompt_tokens` in the error body.
- Fixed a pre-existing test-only borrow in
  `runtime/ai/pg_wire_ask_row_encoder.rs` so the targeted server test
  suite can compile.

Verification:

- `cargo check -p reddb-io-server`
- `cargo test -p reddb-io-server runtime::ai::cost_guard`
- `cargo test -p reddb-io-server runtime::config_matrix`
- `cargo test -p reddb-io-server server::handlers_query::tests::http_query_ask_prompt_token_guard_returns_413_with_limit_name`
- `rustfmt --edition 2021 --check` on touched Rust files except
  `pg_wire_ask_row_encoder.rs`

Deferred to follow-up slices:

- Enforce `max_completion_tokens` and `timeout_ms` around the provider
  call / streamed chunks.
- Add per-tenant daily spend state and UTC reset bookkeeping for
  `daily_cost_cap_usd`.
- Add the provider timeout integration test returning HTTP 504.

Slice 3: post-provider completion and timeout guard wiring landed.

- Provider requests now use `ask.max_completion_tokens` instead of the
  previous hard-coded 1024 cap.
- `execute_ask` measures provider-call elapsed time and re-runs
  `CostGuardEvaluator` after the provider returns with actual
  `completion_tokens` and `elapsed_ms`.
- Completion-token breaches map to HTTP 413 with
  `max_completion_tokens` in the error body.
- Provider elapsed-time breaches map to HTTP 504 with `timeout_ms` in
  the error body.
- Added HTTP handler tests backed by a local OpenAI-compatible TCP stub
  so timeout and completion-token behavior are verified without a real
  provider call.

Verification:

- `cargo test -p reddb-io-server http_query_ask_ -- --nocapture`

Still deferred:

- Per-tenant daily spend state and UTC reset bookkeeping for
  `daily_cost_cap_usd`.
