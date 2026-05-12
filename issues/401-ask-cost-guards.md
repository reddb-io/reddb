# Cost guards: settings + 413/504 errors (CostGuardEvaluator) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/401

Labels: needs-triage

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
