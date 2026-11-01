---
"@reddb-io/cli": patch
---

Avoid profiling unrelated collections and materializing WAL payloads when querying memory-budget statistics. Preserve typed batch-mutation errors so duplicate values in standalone UNIQUE indexes return a client error instead of HTTP 500.
