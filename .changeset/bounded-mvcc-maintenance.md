---
"@reddb-io/cli": patch
---

Reclaim obsolete table history in bounded background batches for writable
standalone embedded runtimes. Avoid copying row payloads during VACUUM and
planner statistics collection, and bound cardinality and histogram working sets.
