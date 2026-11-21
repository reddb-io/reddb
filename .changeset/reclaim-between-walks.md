---
"@reddb-io/cli": patch
---

History reclamation no longer stalls a table behind a long read of it. Removing a row from a collection's disk base excludes a walk of that base. The reclamation tick waited for the walk while holding the publication fence, so every UPDATE queued on the fence and every new read of the collection queued behind the waiting writer until the walk ended. Reclamation now gives a row five milliseconds and leaves the rest for a later pass.
