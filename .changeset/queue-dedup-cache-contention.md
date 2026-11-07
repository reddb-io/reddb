---
"@reddb-io/cli": patch
---

Index queue producer deduplication and expiry inside the database, bound indexed expiry cleanup, and check queue capacity without loading persisted payloads. Preserve source validation for legacy indices and visibility policies. Allow ordinary hot blob-cache hits under shared shard locks and prepare compressed writes before exclusive publication, preserving version and invalidation checks.

Read disk-only queue message payloads and retry limits after bounded cold boot, preserving delivery and ACK recovery.

Publish the visibility/count index alongside the first checkpoint of a new or empty collection. Keep historical backfills incremental, make the index immediately available to GC, and reuse it during cold append checkpoints instead of rereading the old payloads.
