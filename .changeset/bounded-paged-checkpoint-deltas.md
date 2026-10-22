---
"@reddb-io/cli": patch
---

Checkpoint small WAL-backed table updates without rereading and replacing untouched payloads. Keep bounded mutation and WAL-image proofs, conservative fallbacks and the existing publication fences. Preserve the canonical inserted row image in versioned UPDATE WAL records, and keep runtime statistics available when embedded options open a native paged store.
