---
"@reddb-io/cli": patch
---

Use segment key indexes for KV point reads instead of materializing entire collections, while retaining MVCC version selection and recovery behavior.
