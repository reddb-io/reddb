# EXPLAIN ASK shows retrieval plan without LLM call [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/411

Labels: needs-triage

GitHub issue number: #411

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

`EXPLAIN ASK '...'` returns the retrieval plan, source budget allocation, provider selection, and estimated cost — without calling the LLM.

Useful for debugging expensive queries before paying token cost, and for understanding which provider/model would be selected by the failover ladder.

Same options apply (`USING`, `LIMIT`, `MIN_SCORE`, `DEPTH`).

## Acceptance criteria

- [ ] `EXPLAIN ASK '...'` parses and dispatches.
- [ ] Output shows: per-bucket retrieval plan, RRF budget allocation, source URNs that would be selected, chosen provider/model, estimated prompt tokens.
- [ ] No LLM call is made.
- [ ] No audit row written for EXPLAIN.
- [ ] Integration test with stub retrievals.

## Blocked by

- #398
