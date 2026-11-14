---
"@reddb-io/client": patch
---

Use the native RedWire queue-wait frame for single-message `readWait` calls so idle consumers no longer occupy engine execution workers, and expose `readWaitEntries` for delivery metadata.
