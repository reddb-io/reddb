---
"@reddb-io/cli": patch
---

Reuse retired paged checkpoint generations after durable catalog publication,
preserving active readers and retirement roots across restarts. Bound checkpoint
and WAL-reconciliation payload batches, reuse free pages for allocator metadata,
and fence consumed freelist ownership before reuse and superblock recovery.
