---
"@reddb-io/cli": patch
---

The live-version hint also resolves rows a checkpoint moved to disk, so resolving a logical row id stays a point read after eviction.
