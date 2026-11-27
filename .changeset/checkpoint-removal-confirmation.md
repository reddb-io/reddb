---
"@reddb-io/cli": patch
---

A delete of a disk-resident row no longer forces the next checkpoint to publish the whole collection again when a reader happened to hold the collection as the delete was logged. The confirmation that the removal is in the WAL was dropped in that case; it is now kept and applied by the next holder of the backing. The log line of a republished collection also names the exact reason the backing delta was refused.
