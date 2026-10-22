---
"@reddb-io/cli": patch
---

Keep paged checkpoints bounded when committed table updates overlap open transactions, later commits or rollbacks. Track the limited set of omitted or normalized MVCC rows instead of rescanning unchanged payload history, while preserving exact durable WAL-image checks, conservative fallback and publication ordering.
