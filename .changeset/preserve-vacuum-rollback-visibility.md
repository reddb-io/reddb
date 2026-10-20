---
"@reddb-io/cli": patch
---

Preserve aborted transaction outcomes during VACUUM so rolled-back inserts cannot
become visible again, including in collections outside a targeted vacuum.
