---
"@reddb-io/cli": patch
---

Rebuild persisted HASH and BTREE runtime indexes on cold open with a new generation identity. Fall back to source scans while physical index artifacts are not queryable, including when a stale miss exists in the result cache.
