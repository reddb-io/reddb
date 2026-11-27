---
"@reddb-io/cli": patch
---

A page read that misses the cache no longer waits for a checkpoint in progress: it takes the published page from the file. A reader that waited kept the collection it was reading pinned for the whole checkpoint, and the checkpoint then published that collection again in full instead of applying its changes in place. Replacing a row by key, or deleting rows inside a transaction, no longer makes the next checkpoint publish the whole collection again either.
