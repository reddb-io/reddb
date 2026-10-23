---
"@reddb-io/cli": patch
---

Keep checkpoints incremental after small bulk inserts by tracking each inserted row and retaining its segment-assigned sequence identity in the WAL and paged tree. Existing visibility, durability, and bounded-tracking checks remain enforced.
