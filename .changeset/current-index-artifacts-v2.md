---
"@reddb-io/cli": patch
---

Rebuild paged HASH and BTREE artifacts as current-state indexes so MVCC updates and deletes remain discoverable after checkpoint and cold reopen, and store runtime index metadata under the canonical `red.index.registry` control collection.
