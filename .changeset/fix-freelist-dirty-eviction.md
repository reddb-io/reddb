---
"@reddb-io/cli": patch
---

Preserve dirty data pages evicted while publishing freelist trunks during checkpoint. Include them in the WAL-first, double-write-protected flush batch to prevent acknowledged data from reverting after a cold reopen.
