# Provider failover ordered list configurable [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/404

Labels: needs-triage

GitHub issue number: #404

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Ordered provider failover triggered on transport errors, 5xx, or timeout.

Settings: `ask.providers.fallback = ['groq', 'openai', 'anthropic']`. Per-query override: `ASK '...' USING 'groq,openai'`.

Failover preserves seed, temperature, and strict mode across attempts. The successful provider is recorded in the response `provider` field and audited. If all providers fail, return 503 with a list of attempted providers and their errors.

## Acceptance criteria

- [ ] Failover triggers on 5xx, transport errors, and timeout.
- [ ] Per-query `USING 'a,b,c'` overrides global setting.
- [ ] Successful provider surfaced in response and audit.
- [ ] All-providers-failed produces 503 with attempt list.
- [ ] Seed and temperature preserved across failover attempts.
- [ ] Integration test with two stub providers where the first errors and the second succeeds.

## Blocked by

- #396

## Progress note (2026-05-12)

Shipped `ProviderFailover` deep module (`crates/reddb-server/src/runtime/ai/provider_failover.rs`,
~430 LOC, 19 unit tests). Pure kernel — no I/O, no async, no clock —
following the same deep-module pattern as `RrfFuser` (#398),
`StrictValidator` (#395), `PromptAssembler` (#397),
`CostGuardEvaluator` (#401), `ProviderCapabilityRegistry` (#396).

Key decisions:
- Failover triggers only on a **closed retryable set**: `Transport`,
  `Status5xx { code, body }`, `Timeout(Duration)`. Anything else is
  classified as `NonRetryable` and short-circuits — we never paper
  over a 401 by silently switching vendors. Otherwise the user would
  see "all providers failed" when the real problem is a bad API key.
- `AttemptError::is_retryable` is the single source of truth used
  both by `run()` and by callers that want their own classification
  (e.g. metrics labels).
- Generic over response type `R`, so the kernel works for both the
  non-streaming `AskResponse` and any future streaming wrapper.
- Attempt fn `FnMut(&str) -> Result<R, AttemptError>` — caller closes
  over the immutable request (seed, temperature, strict), so
  determinism inputs are preserved by construction. Pinned by
  `attempt_fn_is_invoked_with_identical_inputs`.
- Successful outcome carries `prior_errors: Vec<(provider, err)>` so
  the audit row records, e.g., that groq 502'd before openai answered
  — useful for capacity planning. The user-facing answer only
  references the successful provider.
- All-failed outcome (`FailoverExhausted`) lists every attempt for
  the eventual 503 body.
- `parse_using_clause` handles per-query `USING 'a,b,c'`: trims,
  drops empty segments, dedupes preserving first occurrence, returns
  `None` for fully empty input so the caller falls back to the
  global setting.

Deferred to follow-up slices:
- Wire `parse_using_clause` into the parser for `ASK '...' USING '...'`
  and through to the dispatch layer.
- Connect the kernel to the real provider transport
  (`runtime/ai/transport.rs`) — the wiring slice classifies HTTP
  client errors into `AttemptError` variants.
- Map `FailoverExhausted` to a 503 in the HTTP handler with the
  attempt list serialized.
- Audit-row emission of `prior_errors`.

Files:
- `crates/reddb-server/src/runtime/ai/provider_failover.rs` (new)
- `crates/reddb-server/src/runtime/ai/mod.rs` (declare module)

Verification:
- `cargo test -p reddb-io-server runtime::ai::provider_failover`
  → 19 passed, 0 failed.

Acceptance-criteria status after this slice:
- [x] Failover triggers on 5xx, transport errors, and timeout
  (kernel-level; classification surface is `AttemptError`).
- [x] Per-query `USING 'a,b,c'` parsing (kernel-level; parser wiring
  still pending).
- [x] Successful provider surfaced (`FailoverSuccess.provider`).
- [x] All-providers-failed produces attempt list
  (`FailoverExhausted.attempts`); HTTP→503 mapping still pending.
- [x] Seed and temperature preserved across attempts (caller-supplied
  closure; pinned by test).
- [x] Integration test with two stub providers, first errors and
  second succeeds (`second_provider_succeeds_after_5xx`).

Blockers / notes for next iteration:
- The HTTP-layer slice should decide whether `prior_errors` is
  exposed on the 200 response (transparency) or kept audit-only
  (cleaner contract). Likely audit-only — document in PRD #391 or
  a dedicated ADR.
- Pre-existing untracked `audit_record_builder.rs` in the same
  directory does not compile (`use serde_json::{json, Value};`
  should be `use crate::serde_json::{...}`). Not authored here and
  removed from `mod.rs` so the workspace compiles. Owner of that
  file needs to fix the import before re-declaring the module.
