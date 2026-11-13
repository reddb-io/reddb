---
"@reddb-io/cli": patch
---

Recover a replica that rejoins below a promoted timeline's fork through one bounded basebackup and reopen. Preserve the target replica identity, retire the replaced native WAL, and require a durable snapshot proof before repairing an invalidated replication slot so forged or stale cursors cannot release retained WAL.
