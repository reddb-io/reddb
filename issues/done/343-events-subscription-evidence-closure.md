# Events subscription evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/343

Labels: enhancement

GitHub issue number: #343

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Verify multi-subscription behavior for event-producing collections through observable target queue output, including add/drop subscription and redaction interactions.

Covers: #296

User stories covered: 24

## Acceptance criteria

- [ ] A collection can deliver events to multiple subscriptions with observable output in each target queue.
- [ ] DROP SUBSCRIPTION removes only the named subscription and preserves remaining subscriptions.
- [ ] Redaction and fanout behavior are evidenced or split into explicit follow-up issues.
- [ ] The evidence report no longer marks #296 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
