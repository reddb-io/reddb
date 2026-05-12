# Ratify MVCC history-store contract [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/433

Labels: hitl

GitHub issue number: #433

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Ratify the MVCC history-store and transaction recovery contract from ADR 0014 before implementation begins. This slice should turn the draft architecture into an agreed implementation contract: logical identity vs physical version identity, global history store shape, transaction commit-batch semantics, first-committer-wins conflict policy, index recheck rule, and the exact guarantees RedDB will document for table rows in the first rollout.

## Acceptance criteria

- [ ] ADR 0014 is reviewed and either accepted or updated with the final contract for logical identity, history store keys, commit ordering, and MVCC resolver semantics.
- [ ] Open questions in the ADR that block implementation are resolved or explicitly deferred with owner-visible rationale.
- [ ] The PRD scope remains table-row-first and explicitly does not claim serializable isolation, autovacuum, historical indexes, or full multi-model rollout.
- [ ] The accepted contract names the first implementation slices that can proceed AFK.

## Blocked by

None - can start immediately.
