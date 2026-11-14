---
"@reddb-io/cli": patch
---

Allow canonical configuration recovery to replay pending WAL when a historical database contains only the populated `red_config` resume identity. Promote that single root to `red.config`, normalize its legacy keys, and preserve the source database and WAL unchanged.
