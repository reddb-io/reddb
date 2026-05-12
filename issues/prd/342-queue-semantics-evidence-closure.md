# Queue semantics evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/342

Labels: prd

GitHub issue number: #342

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Prove FANOUT runtime semantics and ALTER QUEUE SET MODE transition behavior using consumer-visible queue behavior, including active-consumer warning semantics where applicable.

Covers: #287, #289

User stories covered: 22, 23

## Acceptance criteria

- [ ] FANOUT semantics are evidenced by multiple consumers receiving expected messages independently.
- [ ] ALTER QUEUE SET MODE behavior is evidenced for active/in-flight consumers or split into a missing warning-contract issue.
- [ ] The evidence report no longer marks #287 or #289 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
