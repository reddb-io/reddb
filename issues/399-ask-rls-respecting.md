# RLS-respecting ASK retrieval [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/399

Labels: enhancement

GitHub issue number: #399

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

ASK retrieval runs through the same authorization-aware executor that backs SELECT. The caller's user/role context is propagated into every retrieval call (BM25, vector, graph).

After this slice, a row, document, vector, or graph element that a SELECT would not return for the caller cannot appear in ASK `sources_flat` for that caller. The audit row records the role used.

No new public API — this is a correctness/security fix to the retrieval layer.

## Acceptance criteria

- [ ] RLS policies on tables are applied during ASK retrieval.
- [ ] Row-level visibility on documents/KV applied.
- [ ] Vector and graph retrievals filter by collection-level grants.
- [ ] Audit row records the role used.
- [ ] Integration test: two users querying ASK on the same dataset receive different `sources_flat` matching their grants.
- [ ] Negative test: a user with no read grant on a table cannot leak its content via ASK.

## Blocked by

- #398
