---
"@reddb-io/cli": patch
---

Cancel cooperative query execution when an HTTP request deadline expires so
blocking workers release their global and per-principal capacity permits.
