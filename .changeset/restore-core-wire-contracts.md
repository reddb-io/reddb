---
"@reddb-io/cli": patch
---

Stabilize RedWire query-summary bytes, include the auditable ASK planner trace
and final-query fields in typed gRPC replies, and make primary-replica WAL
publication idempotent when replaying a committed transaction outbox.
