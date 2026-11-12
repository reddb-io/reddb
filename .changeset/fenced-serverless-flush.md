---
"@reddb-io/cli": patch
---

Fence explicit runtime flushes with the serverless writer lease so a losing instance cannot overwrite the active writer's remote database snapshot.
