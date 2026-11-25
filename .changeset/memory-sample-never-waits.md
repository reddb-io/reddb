---
"@reddb-io/cli": patch
---

Memory accounting no longer waits for segment locks. Every reservation and release samples the resident bytes of all collections, including from inside the callbacks of a scan that already holds a collection's read locks; with a writer queued on one of them the sample parked behind the writer while the writer waited for the scan. The sample now takes the locks only if they are free for it and otherwise reports the last value it measured, so it cannot join a lock cycle or wait behind a long write.
