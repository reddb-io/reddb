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
