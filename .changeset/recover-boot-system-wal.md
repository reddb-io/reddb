---
"@reddb-io/cli": patch
---

Recover a canonical system-config copy when its pending WAL only targets proved boot-derived legacy metadata. Preserve all other transactions, original WAL framing and consumed identities; reject unknown deletes and user configuration changes.
