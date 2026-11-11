---
"@reddb-io/cli": patch
---

Stabilize RedWire query-summary bytes, include the auditable ASK planner trace
and final-query fields in typed gRPC replies, and make primary-replica WAL
publication idempotent when replaying a committed transaction outbox.

Bound HASH and index-intersection candidate memory, recover torn WAL tails before
new appends, preserve the first user entity ID when the durable result-cache epoch
is created, and keep non-read statements from initializing the result-cache format.
Harden embedded WAL replay and transactional reservation regression coverage.
