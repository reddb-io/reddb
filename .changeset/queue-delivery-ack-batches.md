---
"@reddb-io/cli": patch
---

Batch unkeyed queue delivery reservations and attempt counters, and add bounded multi-message ACK with one durable autocommit publication. Preserve ordered-key, retry, FANOUT and explicit-transaction behavior, with failure rollback, recovery and cold-boot coverage. Verify queue/table relevance joins without consuming messages.
