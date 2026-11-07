---
"@reddb-io/cli": patch
---

Index queue producer deduplication and expiry inside the database, bound indexed expiry cleanup, and check queue capacity without loading persisted payloads. Preserve source validation for legacy indices and visibility policies. Allow ordinary hot blob-cache hits under shared shard locks and prepare compressed writes before exclusive publication, preserving version and invalidation checks.
