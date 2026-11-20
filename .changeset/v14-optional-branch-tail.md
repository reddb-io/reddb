---
"@reddb-io/cli": patch
---

Fix `truncated native payload` when replaying WAL records, restoring backups or applying replication frames written before store format V14: the branch provenance tail is now optional on read.
