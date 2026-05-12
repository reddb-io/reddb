# SDK and Redis migration tooling closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/340

Labels: enhancement

GitHub issue number: #340

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Prove Python cache get/put/invalidate public behavior and reconcile the real status of the red migrate-from-redis dual-write tool. Split any missing tool behavior into narrow follow-ups.

Covers: #197, #199

User stories covered: 15, 16

## Acceptance criteria

- [ ] Python SDK cache get/put/invalidate methods have public API tests or documented current equivalent behavior.
- [ ] red migrate-from-redis status is explicit: implemented and verified, superseded, or split into a new implementation issue.
- [ ] The evidence report no longer marks #197 or #199 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
