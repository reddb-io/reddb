---
"@reddb-io/cli": patch
---

Isolate checkpoint WAL-image eviction by collection while preserving the shared memory budget. Later writes in another collection no longer force a full payload-tree rewrite when their own row or byte cache fills. Exact WAL-image validation, publication fences, and persistent formats are unchanged.
