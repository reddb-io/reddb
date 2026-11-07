---
"@reddb-io/cli": patch
---

Allow old native catalogs without resume counters to reopen when a legacy system collection has a zero-root canonical alias. Preserve the populated tree and its records; continue rejecting two populated roots. Cover cold reopen and checkpoint recovery.
