---
"@reddb-io/cli": patch
---

Reduce embedded cold-start I/O by reusing the validated snapshot and publishing boot-time system configuration in one WAL batch.
