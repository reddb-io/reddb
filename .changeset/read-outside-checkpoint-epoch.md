---
"@reddb-io/cli": patch
---

A page read that misses the cache no longer waits for a checkpoint in progress: it takes the published page from the file. A reader that waited kept the collection it was reading pinned for the whole checkpoint, and the checkpoint then published that collection again in full instead of applying its changes in place. A checkpoint also waits for a scan in progress in proportion to the size of the collection, and for a lookup to leave an index, before it falls back to publishing the collection again. Replacing a row by key, or deleting rows inside a transaction, no longer makes the next checkpoint publish the whole collection again either. The log line of a republished collection tells a changed index definition from a reader on an index and from a projection still being built.
