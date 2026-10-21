---
"@reddb-io/cli": patch
---

Fence bulk row insertion through its WAL capture or append so a checkpoint cannot publish an unfinished batch that starts during serialization. Add forced-crash regressions for explicit transactions and acknowledged autocommit batches.
