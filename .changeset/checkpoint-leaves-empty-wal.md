---
"@reddb-io/cli": patch
---

A checkpoint again leaves the WAL empty: the physical metadata record it publishes is folded into the same checkpoint, so offline compaction and scrub accept a checkpointed store.
