---
"@reddb-io/cli": patch
---

Keep a failed consumed-freelist-trunk fence pending across allocation retries.
Synchronize both superblocks before reusing any loaded free page after an I/O
error, so retrying an allocation cannot bypass the durable ownership change.
