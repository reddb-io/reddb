---
"@reddb-io/cli": patch
---

Fence a WAL writer after a failed write or synchronous durability barrier, including disk exhaustion after a partial batch. Prevent retries and destructor flushes from corrupting WAL framing or publishing rejected buffered transactions. Stop the failed group-commit worker and require reopening for recovery before accepting further writes.

Roll back resident rows from failed single and bulk insert WAL publications so scans do not expose rejected inserts while the database remains open.
