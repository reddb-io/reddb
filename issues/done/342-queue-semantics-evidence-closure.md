# Queue semantics evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/342

Labels: enhancement

GitHub issue number: #342

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Prove FANOUT runtime semantics and ALTER QUEUE SET MODE transition behavior using consumer-visible queue behavior, including active-consumer warning semantics where applicable.

Covers: #287, #289

User stories covered: 22, 23

## Acceptance criteria

- [x] FANOUT semantics are evidenced by multiple consumers receiving expected messages independently.
- [x] ALTER QUEUE SET MODE behavior is evidenced for active/in-flight consumers or split into a missing warning-contract issue.
- [x] The evidence report no longer marks #287 or #289 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)

## Closure

- Confirmed #287 from public runtime coverage in `tests/integration_queue_timeseries.rs` for FANOUT broadcast delivery, per-consumer ACK isolation, and per-consumer DLQ isolation.
- Confirmed #289 from public runtime coverage for WORK to FANOUT transition behavior: in-flight WORK messages remain ackable through `_work_default`, while new reads use FANOUT delivery. Runtime also emits the pending-count tracing warning for active pending messages.
- Added `scripts/queue_semantics_contract.test.mjs` and report-disposition coverage so generated evidence ledgers no longer leave #287 or #289 with a partial placeholder disposition.
