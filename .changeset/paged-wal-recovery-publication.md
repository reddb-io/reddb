---
"@reddb-io/cli": patch
---

Replay committed paged WAL actions into memory before building replacement trees, and synchronize replacement pages before publishing their catalog roots. Interrupted recovery no longer spills intermediate historical tree mutations over the published checkpoint. Unchanged trees are retained, auxiliary WAL records remain available to runtime recovery, and old metadata overflow pages are recycled only after the replacement catalog is durable.
