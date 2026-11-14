---
"@reddb-io/cli": patch
---

Recover pending WAL for historical disk-resident databases with a single `red_config` root, promoting that root to `red.config` on the isolated copy while preserving the source database and WAL unchanged.
