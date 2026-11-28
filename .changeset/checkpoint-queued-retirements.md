---
"@reddb-io/cli": patch
---

A checkpoint no longer waits for one WAL sync per page it frees. While the new generation is unpublished, recovery ignores its retirements, so those records are queued behind the reservations and made durable with them before the manifest is written; a checkpoint that changed 20 of 173 small tables went from about 400 fsync calls to about 155. Retirements recorded after publication are awaited as before.
