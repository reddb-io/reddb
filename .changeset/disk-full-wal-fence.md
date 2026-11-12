---
"@reddb-io/cli": patch
---

Fence a WAL writer after a failed write, including disk exhaustion after a partial batch. Prevent retries and destructor flushes from corrupting WAL framing or publishing rejected buffered transactions. Stop the failed group-commit worker and require reopening for recovery before accepting further writes.
