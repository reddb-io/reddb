# Define public surface contract matrix for feedback-driven release quality [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/450

## What to build

Create the public-surface contract matrix for RedDB product quality. The matrix must enumerate every public promise from README, docs/query, driver READMEs, examples, and the feedback scenarios, then map each promise to an automated test or an explicit documentation correction.

This is the oldest unblocked GitHub issue from the feedback-quality PRD. Keep the change small and focused: add the matrix artifact and enough supporting documentation/tests links to make the next issues actionable.

## Acceptance Criteria

- [x] A contract matrix exists in the repo and identifies the source of each public promise.
- [x] Each promise is classified as passing, failing, missing test coverage, or intentionally unsupported.
- [x] Each feedback-derived scenario from `../feedbacks.md` and `../feedbacks-new.md` is represented.
- [x] The matrix distinguishes public docs/examples/drivers from ADR/internal future design.
- [x] The matrix defines the minimum conformance layer for each feature: runtime/parser, HTTP, persistence, transport smoke, or SDK.

## Verification

- `rtk cargo test --test public_surface_contract_matrix`

## Blocked by

None - can start immediately.
