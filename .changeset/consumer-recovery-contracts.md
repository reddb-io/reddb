---
"@reddb-io/cli": patch
---

Preserve snapshot-visible rows during secondary-index publication and logical replay. Persist replayed version changes in WAL, retain table-delete tombstones, and update indexes before advancing the replica frontier. Keep embedded read-only opens and legacy snapshots unchanged, detect Windows memory, and preserve client projection and cancellation contracts. Interrupted RedWire connections must be reopened without automatically retrying writes; unsupported read-only native paged recovery now fails before writing.
